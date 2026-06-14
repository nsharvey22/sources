#![no_std]
extern crate alloc;

use aidoku::imports::canvas::ImageRef;
use aidoku::{
	Chapter, DeepLinkHandler, DeepLinkResult, FilterValue, HashMap, Home, HomeComponent,
	HomeLayout, HomePartialResult, ImageRequestProvider, ImageResponse, Link, LinkValue, Listing,
	ListingProvider, Manga, MangaPageResult, MangaWithChapter, NotificationHandler, Page,
	PageContent, PageContext, PageImageProcessor, Result, Source,
	alloc::{String, Vec, string::ToString, vec},
	helpers::uri::QueryParameters,
	imports::{
		net::Request,
		std::send_partial_result,
	},
	prelude::*,
};

mod descramble;
mod helpers;
mod models;
mod settings;
mod web;

use models::*;

const BASE_URL: &str = "https://comix.to";

// adult, boys love, ecchi, girls love, hentai, smut
const NSFW_GENRE_IDS: &[&str] = &["87264", "8", "87265", "13", "87266", "87268"];

struct Comix;

impl Source for Comix {
	fn new() -> Self {
		Self
	}

	fn get_search_manga_list(
		&self,
		query: Option<String>,
		page: i32,
		filters: Vec<FilterValue>,
	) -> Result<MangaPageResult> {
		let mut qs = QueryParameters::new();
		qs.push("page", Some(&page.to_string()));
		if query.is_some() {
			qs.push("q", query.as_deref());
		}

		let mut hidden_types = {
			let types = settings::hidden_types();
			if types.is_empty() { None } else { Some(types) }
		};
		let mut hidden_terms = settings::hidden_terms();

		let mut has_sort_filter = false;

		for filter in filters {
			match filter {
				FilterValue::Text { .. } => {
					// Term lookups require a token-gated API endpoint; skip for now.
				}
				FilterValue::Sort { index, ascending, .. } => {
					// index 1 = "Latest Updates" — no sort param, just omit it.
					let field = match index {
						0 => "relevance",
						1 => "",
						2 => "created_at",
						3 => "title",
						4 => "year",
						5 => "score",
						6 => "views_7d",
						7 => "views_30d",
						8 => "views_90d",
						9 => "views_total",
						10 => "follows_total",
						_ => "relevance",
					};
					if !field.is_empty() {
						let dir = if ascending { "asc" } else { "desc" };
						qs.push("sort", Some(&format!("{field}:{dir}")));
					}
					has_sort_filter = true;
				}
				FilterValue::Select { id, value } => {
					qs.push(&id, Some(&value));
				}
				FilterValue::MultiSelect {
					id,
					included,
					excluded,
				} => {
					if id == "types[]" {
						hidden_types = None;
					}
					for value in included {
						if id == "genres[]" {
							let id_num = value.parse::<i32>().unwrap_or_default();
							if let Some(pos) = hidden_terms.iter().position(|&x| x == id_num) {
								hidden_terms.swap_remove(pos);
								continue;
							}
							qs.push("genres_in[]", Some(&value));
						} else {
							qs.push(&id, Some(&value));
						}
					}
					for value in excluded {
						if id == "genres[]" {
							if hidden_terms.contains(&value.parse().unwrap_or_default()) {
								continue;
							}
							qs.push("genres_ex[]", Some(&value));
						} else {
							qs.push(&id, Some(&format!("-{value}")));
						}
					}
				}
				_ => continue,
			}
		}

		if !has_sort_filter {
			qs.push("sort", Some("relevance:desc"));
		}

		if let Some(hidden_types) = hidden_types {
			for &typ in &["manga", "manhwa", "manhua", "other"] {
				if !hidden_types.iter().any(|s| s.as_str() == typ) {
					qs.push("types[]", Some(typ));
				}
			}
		}

		for term in hidden_terms {
			qs.push("genres_ex[]", Some(&term.to_string()));
		}

		if settings::hide_nsfw() {
			for genre_id in NSFW_GENRE_IDS {
				qs.push("genres_ex[]", Some(genre_id));
			}
		}

		qs.push("content_rating", Some("suggestive"));

		let url = format!("{BASE_URL}/browse?{qs}");
		let raw = web::fetch_manga_list_data(&url)?;
		let hidden_types = settings::hidden_types();
		let hidden_terms = settings::hidden_terms();
		serde_json::from_str::<SearchResponse>(&raw)
			.map(|r| r.result.into_filtered(&hidden_types, &hidden_terms))
			.map_err(|e| error!("{e}"))
	}

	fn get_manga_update(
		&self,
		mut manga: Manga,
		needs_details: bool,
		needs_chapters: bool,
	) -> Result<Manga> {
		if needs_details {
			let title_url = manga
				.url
				.clone()
				.unwrap_or_else(|| format!("{BASE_URL}/title/{}", manga.key));
			println!("[comix] get_manga_update: fetching details for {title_url}");
			let raw = web::fetch_manga_detail_data(&title_url)?;
			let json: SingleMangaResponse = serde_json::from_str(&raw)?;
			manga.copy_from(json.result.into());
			if needs_chapters {
				send_partial_result(&manga);
			}
		}

		if needs_chapters {
			let mut page = 1;
			let deduplicate = settings::dedupchapter();
			let mut chapter_map: HashMap<String, ComixChapter> = HashMap::new();
			let mut chapter_list: Vec<ComixChapter> = Vec::new();

			let manga_web_url = manga
				.url
				.clone()
				.unwrap_or_else(|| format!("{BASE_URL}/title/{}", manga.key));

			loop {
				let page_url = format!("{manga_web_url}?page={page}");
				let result = web::fetch_chapter_data(&page_url)?;
				let res = serde_json::from_str::<ChapterDetailsResponse>(&result)?;

				let items = res.result.items;

				if deduplicate {
					for item in items {
						helpers::dedup_insert(&mut chapter_map, item);
					}
				} else {
					chapter_list.extend(items);
				}

				if res.result.meta.page >= res.result.meta.last_page {
					break;
				}

				page += 1;
			}

			let mut chapters: Vec<Chapter> = if deduplicate {
				chapter_map.into_values().map(Into::into).collect()
			} else {
				chapter_list.into_iter().map(Into::into).collect()
			};

			if deduplicate {
				chapters.sort_by(|a, b| {
					b.chapter_number
						.partial_cmp(&a.chapter_number)
						.unwrap_or(core::cmp::Ordering::Equal)
				});
			}

			manga.chapters = Some(chapters);
		}

		Ok(manga)
	}

	fn get_page_list(&self, _manga: Manga, chapter: Chapter) -> Result<Vec<Page>> {
		println!("[comix] get_page_list: key={} url={:?}", chapter.key, chapter.url);
		let chapter_url = chapter
			.url
			.as_deref()
			.ok_or_else(|| error!("Missing chapter URL"))?;
		let result = web::fetch_page_list_data(chapter_url)?;
		let json: ChapterResponse = serde_json::from_str(&result)?;

		let Some(result) = json.result else {
			bail!("Missing chapter")
		};

		let base_url = result.pages.base_url.trim_end_matches('/');
		Ok(result
			.pages
			.items
			.into_iter()
			.map(|page| {
				let url = if page.url.starts_with("http") {
					page.url
				} else {
					format!("{base_url}/{}", page.url.trim_start_matches('/'))
				};
				Page {
					content: if let Some(s) = page.s {
						let mut context = PageContext::new();
						context.insert("s".into(), s.to_string());
						PageContent::url_context(url, context)
					} else {
						PageContent::url(url)
					},
					..Default::default()
				}
			})
			.collect())
	}
}

impl Home for Comix {
	fn get_home(&self) -> Result<HomeLayout> {
		println!("[comix] get_home: called");

		let extra_qs = if settings::hide_nsfw() {
			NSFW_GENRE_IDS
				.iter()
				.map(|id| format!("&genres_ex[]={id}"))
				.collect::<String>()
		} else {
			Default::default()
		};

		let hidden_types = settings::hidden_types();
		let hidden_terms = settings::hidden_terms();

		// (title, browse query params, is_chapter_list_style)
		let browse_sections: &[(&str, &str, bool)] = &[
			("Popular", "sort=score:desc&limit=20&content_rating=suggestive", false),
			(
				"Most Followed",
				"sort=follows_total:desc&limit=20&content_rating=suggestive",
				false,
			),
			// Latest Updates uses no sort param per comix.to browse conventions.
			("Latest Updates", "limit=20&content_rating=suggestive", false),
			(
				"Hot Webtoons",
				"types[]=manhwa&scope=hot&limit=20&content_rating=suggestive",
				false,
			),
			(
				"Recently Added",
				"sort=created_at:desc&limit=10&content_rating=suggestive",
				true,
			),
		];

		// Send initial layout with empty placeholders.
		send_partial_result(&HomePartialResult::Layout(HomeLayout {
			components: browse_sections
				.iter()
				.map(|(title, _, is_list)| HomeComponent {
					title: Some((*title).into()),
					subtitle: None,
					value: if *is_list {
						aidoku::HomeComponentValue::empty_manga_chapter_list()
					} else {
						aidoku::HomeComponentValue::empty_scroller()
					},
				})
				.collect(),
		}));

		for (title, params, is_list) in browse_sections {
			let url = format!("{BASE_URL}/browse?{params}{extra_qs}");
			let raw = match web::fetch_manga_list_data(&url) {
				Ok(r) => r,
				Err(e) => {
					println!("[comix] get_home: failed to load '{title}': {e:?}");
					continue;
				}
			};
			let items = match serde_json::from_str::<SearchResponse>(&raw) {
				Ok(r) => r
					.result
					.items
					.into_iter()
					.filter(|m| !m.is_hidden(&hidden_types, &hidden_terms))
					.collect::<Vec<_>>(),
				Err(e) => {
					println!("[comix] get_home: parse error for '{title}': {e}");
					continue;
				}
			};

			if *is_list {
				let entries = items
					.into_iter()
					.map(|m| {
						let chapter_number = m.latest_chapter;
						let manga = Manga::from(m);
						MangaWithChapter {
							manga,
							chapter: Chapter {
								chapter_number,
								..Default::default()
							},
						}
					})
					.collect();
				send_partial_result(&HomePartialResult::Component(HomeComponent {
					title: Some((*title).into()),
					subtitle: None,
					value: aidoku::HomeComponentValue::MangaChapterList {
						page_size: None,
						entries,
						listing: Some(Listing {
							id: (*title).into(),
							name: (*title).into(),
							..Default::default()
						}),
					},
				}));
			} else {
				let entries = items
					.into_iter()
					.map(|m| {
						let manga = Manga::from(m);
						Link {
							title: manga.title.clone(),
							subtitle: None,
							image_url: manga.cover.clone(),
							value: Some(LinkValue::Manga(manga)),
						}
					})
					.collect();
				send_partial_result(&HomePartialResult::Component(HomeComponent {
					title: Some((*title).into()),
					subtitle: None,
					value: aidoku::HomeComponentValue::Scroller {
						entries,
						listing: Some(Listing {
							id: (*title).into(),
							name: (*title).into(),
							..Default::default()
						}),
					},
				}));
			}
		}

		Ok(HomeLayout::default())
	}
}

impl ListingProvider for Comix {
	fn get_manga_list(&self, listing: Listing, page: i32) -> Result<MangaPageResult> {
		let extra_qs = if settings::hide_nsfw() {
			NSFW_GENRE_IDS
				.iter()
				.map(|id| format!("&genres_ex[]={id}"))
				.collect::<String>()
		} else {
			Default::default()
		};
		let hidden_types = settings::hidden_types();
		let hidden_terms = settings::hidden_terms();

		let trending = |types: Vec<String>| {
			self.get_search_manga_list(
				None,
				page,
				vec![
					FilterValue::Sort {
						id: "order".into(),
						index: 8, // most views 1mo
						ascending: false,
					},
					FilterValue::MultiSelect {
						id: "types[]".into(),
						included: types,
						excluded: Default::default(),
					},
				],
			)
		};

		let fetch_listing = |params: &str| -> Result<MangaPageResult> {
			let url = format!("{BASE_URL}/browse?{params}&page={page}{extra_qs}");
			let raw = web::fetch_manga_list_data(&url)?;
			serde_json::from_str::<SearchResponse>(&raw)
				.map(|r| r.result.into_filtered(&hidden_types, &hidden_terms))
				.map_err(|e| error!("{e}"))
		};

		match listing.id.as_str() {
			"Trending Webtoon" => trending(vec!["manhua".into(), "manhwa".into()]),
			"Trending Manga" => trending(vec!["manga".into()]),
			"Popular" => fetch_listing("sort=score:desc&limit=30&content_rating=suggestive"),
			"Most Followed" => {
				fetch_listing("sort=follows_total:desc&limit=30&content_rating=suggestive")
			}
			"Latest Updates" => fetch_listing("limit=30&content_rating=suggestive"),
			"Hot Webtoons" => {
				fetch_listing("types[]=manhwa&scope=hot&limit=30&content_rating=suggestive")
			}
			"Recently Added" => {
				fetch_listing("sort=created_at:desc&limit=30&content_rating=suggestive")
			}
			_ => bail!("Unknown listing"),
		}
	}
}

impl ImageRequestProvider for Comix {
	fn get_image_request(&self, url: String, _context: Option<PageContext>) -> Result<Request> {
		Ok(Request::get(url)?.header("Referer", &format!("{BASE_URL}/")))
	}
}

impl PageImageProcessor for Comix {
	fn process_page_image(
		&self,
		response: ImageResponse,
		context: Option<PageContext>,
	) -> Result<ImageRef> {
		// Only pages flagged with s=1 from the chapter API may need processing.
		let is_scrambled = context
			.as_ref()
			.and_then(|c| c.get("s"))
			.is_some_and(|s| s != "0");
		if !is_scrambled {
			return Ok(response.image);
		}

		// Read encoding / scramble settings from the HTTP response headers.
		let enc_seed = header_i32(&response.headers, "x-enc-seed");
		let enc_len = header_usize(&response.headers, "x-enc-len");
		let enc_algo = header_str(&response.headers, "x-enc-algo");
		let scramble_seed = header_i32(&response.headers, "x-scramble-seed");
		let scramble_grid = header_str(&response.headers, "x-scramble-grid");
		let scramble_algo = header_str(&response.headers, "x-scramble-algo");

		let needs_xor = enc_seed.map(|s| s != 0).unwrap_or(false) && enc_len.is_some();
		let should_descramble = scramble_grid.as_deref() == Some("5x5")
			&& scramble_seed.map(|s| s != 0).unwrap_or(false)
			&& matches!(
				scramble_algo.as_deref(),
				None | Some("1") | Some("2") | Some("3")
			);

		if !needs_xor && !should_descramble {
			return Ok(response.image);
		}

		// When XOR encoding is in play the framework cannot decode the raw bytes
		// as an image, so we re-fetch and XOR-decode them ourselves.
		let image = if needs_xor {
			let url = response.request.url.ok_or(error!("Missing image URL"))?;
			let raw = Request::get(&url)?
				.header("Referer", &format!("{BASE_URL}/"))
				.data()?;
			let decoded =
				descramble::decode_xor(&raw, enc_seed.unwrap(), enc_len.unwrap(), enc_algo.as_deref());
			ImageRef::new(&decoded)
		} else {
			// No XOR: the framework already decoded a valid (but scrambled) image.
			response.image
		};

		if should_descramble {
			Ok(descramble::descramble_tiles(
				&image,
				scramble_seed.unwrap(),
				scramble_algo.as_deref(),
			))
		} else {
			Ok(image)
		}
	}
}

fn header_i32(headers: &HashMap<String, String>, name: &str) -> Option<i32> {
	headers
		.iter()
		.find(|(k, _)| k.eq_ignore_ascii_case(name))
		.and_then(|(_, v)| v.parse::<i64>().ok())
		.map(|v| v as i32)
}

fn header_usize(headers: &HashMap<String, String>, name: &str) -> Option<usize> {
	headers
		.iter()
		.find(|(k, _)| k.eq_ignore_ascii_case(name))
		.and_then(|(_, v)| v.parse::<usize>().ok())
}

fn header_str(headers: &HashMap<String, String>, name: &str) -> Option<String> {
	headers
		.iter()
		.find(|(k, _)| k.eq_ignore_ascii_case(name))
		.map(|(_, v)| v.clone())
}

impl NotificationHandler for Comix {
	fn handle_notification(&self, notification: String) {
		if notification == "resetFilters" {
			settings::reset_filters();
		}
	}
}

impl DeepLinkHandler for Comix {
	fn handle_deep_link(&self, url: String) -> Result<Option<DeepLinkResult>> {
		let Some(path) = url.strip_prefix(&format!("{BASE_URL}/")) else {
			return Ok(None);
		};

		// ex: https://comix.to/title/pvry-one-piece
		// ex: https://comix.to/title/pvry-one-piece/5498414-chapter-1

		let mut segments = path.split('/');

		if let (Some("title"), Some(manga_segment)) = (segments.next(), segments.next()) {
			// ex: pvry-one-piece -> pvry
			let manga_key = manga_segment.split('-').next().unwrap_or(manga_segment);

			if let Some(chapter_segment) = segments.next() {
				// ex: 5498414-chapter-1 -> 5498414
				let chapter_key = chapter_segment.split('-').next().unwrap_or("");
				return Ok(Some(DeepLinkResult::Chapter {
					manga_key: manga_key.to_string(),
					key: chapter_key.to_string(),
				}));
			} else {
				return Ok(Some(DeepLinkResult::Manga {
					key: manga_key.to_string(),
				}));
			}
		}

		Ok(None)
	}
}

register_source!(
	Comix,
	Home,
	ListingProvider,
	ImageRequestProvider,
	PageImageProcessor,
	NotificationHandler,
	DeepLinkHandler
);
