#![no_std]
use aidoku::{
	Chapter, DeepLinkHandler, DeepLinkResult, FilterValue, HashMap, Home, HomeComponent,
	HomeLayout, HomePartialResult, ImageRequest, ImageRequestProvider, ImageResponse, Link,
	LinkValue, Listing, ListingProvider, Manga, MangaPageResult, MangaWithChapter,
	NotificationHandler, Page, PageContent, PageContext, PageImageProcessor, Result, Source,
	alloc::{String, Vec, string::ToString, vec},
	helpers::uri::{QueryParameters, encode_uri_component},
	imports::{
		canvas::{Canvas, ImageRef, Rect},
		net::{Request, Response},
		std::send_partial_result,
	},
	prelude::*,
};
use core::cell::RefCell;

mod helpers;
mod models;
mod settings;
mod web;

use crate::helpers::create_request_get;
use models::*;
use web::*;

const CONTENT_TYPES: &[&str] = &["manga", "manhwa", "manhua", "other"];
// adult, boys love, ecchi, girls love, hentai, smut
const NSFW_GENRE_IDS: &[&str] = &["87264", "8", "87265", "13", "87266", "87268"];

struct Comix {
	web_view: RefCell<ComixWebView>,
}

fn selected_source_url(url: Option<&str>, fallback_path: &str) -> String {
	let path = url
		.and_then(|url| url.split_once("://"))
		.and_then(|(_, rest)| rest.split_once('/'))
		.map(|(_, path)| path)
		.unwrap_or(fallback_path)
		.trim_start_matches('/');
	format!("{}/{path}", settings::base_url())
}

fn fetch_search_response(
	web_view: &mut ComixWebView,
	signed_url: &str,
	browse_url: &str,
) -> Result<SearchResponse> {
	if web_view.has_signer()
		&& let Ok(response) = web_view
			.build_request(signed_url)
			.and_then(|request| Ok(request.send()?))
		&& let Ok(result) = web_view.decode_json_owned::<SearchResponse>(&response)
	{
		return Ok(result);
	}
	web_view.fetch_browse(browse_url)
}

impl Source for Comix {
	fn new() -> Self {
		Self {
			web_view: RefCell::new(ComixWebView::new()),
		}
	}

	fn get_search_manga_list(
		&self,
		query: Option<String>,
		page: i32,
		filters: Vec<FilterValue>,
	) -> Result<MangaPageResult> {
		let mut web_view = self.web_view.borrow_mut();
		let api_url = settings::api_url();

		let mut qs = QueryParameters::new();
		qs.push("page", Some(&page.to_string()));
		if query.is_some() {
			qs.push("keyword", query.as_deref());
		}

		let mut hidden_types = {
			let types = settings::hidden_types();
			if types.is_empty() { None } else { Some(types) }
		};
		let mut hidden_terms = settings::hidden_terms();

		let mut has_sort_filter = false;

		for filter in filters {
			match filter {
				FilterValue::Text { id, value } => {
					let url = format!(
						"{api_url}/tags/search?type={id}&q={}",
						encode_uri_component(value)
					);
					let response = create_request_get(&url)?.send()?;
					let term_id =
						serde_json::from_str::<TagSearchResponse>(&response.get_string()?)?
							.result
							.first()
							.map(|t| t.id)
							.ok_or_else(|| error!("No matching {id}s"))?;
					qs.push(&format!("{id}s[]"), Some(&term_id.to_string()));
				}
				FilterValue::Sort {
					id,
					index,
					ascending,
				} => {
					qs.push(
						&format!(
							"{id}[{}]",
							match index {
								0 => "relevance",
								1 => "chapter_updated_at",
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
							}
						),
						Some(if (index == 3 && !ascending) || (index != 3 && ascending) {
							"asc"
						} else {
							"desc"
						}),
					);
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
					// if any content type is set manually, skip our content type filters
					if id == "types[]" {
						hidden_types = None;
					}
					for value in included {
						// if a hidden term is manually included in filters, skip hiding it
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
						// make sure hidden terms aren't added to query params twice
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
			qs.push("order[relevance]", Some("desc"));
		}

		if let Some(hidden_types) = hidden_types {
			for &typ in CONTENT_TYPES {
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

		let signed_url = format!("{api_url}/manga?{qs}");
		let browse_url = format!("{}/browse?{qs}", settings::base_url());
		fetch_search_response(&mut web_view, &signed_url, &browse_url).map(Into::into)
	}

	fn get_manga_update(
		&self,
		mut manga: Manga,
		needs_details: bool,
		needs_chapters: bool,
	) -> Result<Manga> {
		let mut web_view = self.web_view.borrow_mut();
		let api_url = settings::api_url();

		if needs_details {
			let signed_url = format!(
				"{api_url}/manga/{}?includes[]=demographic\
									&includes[]=genre\
									&includes[]=theme\
									&includes[]=author\
									&includes[]=artist\
									&includes[]=publisher",
				manga.key
			);
			let page_url =
				selected_source_url(manga.url.as_deref(), &format!("title/{}", manga.key));
			let detail = if web_view.has_signer() {
				web_view
					.build_request(&signed_url)
					.and_then(|request| Ok(request.send()?))
					.and_then(|response| {
						web_view.decode_json_owned::<SingleMangaResponse>(&response)
					})
					.map(|response| response.result)
					.or_else(|_| web_view.fetch_manga(&page_url))?
			} else {
				web_view.fetch_manga(&page_url)?
			};

			manga.copy_from(detail.into());

			if needs_chapters {
				send_partial_result(&manga);
			}
		}

		if needs_chapters {
			let deduplicate = settings::dedupchapter();
			let mut chapter_map: HashMap<String, ComixChapter> = HashMap::new();
			let page_url =
				selected_source_url(manga.url.as_deref(), &format!("title/{}", manga.key));
			let has_signer = web_view.has_signer();
			let mut fetch_signed = || -> Result<Vec<ComixChapter>> {
				let mut page = 1;
				let mut chapters = Vec::new();
				loop {
					let mut params = QueryParameters::new();
					params.push("limit", Some("100"));
					params.push("page", Some(page.to_string().as_str()));
					params.push("order[number]", Some("desc"));
					let url = format!("{api_url}/manga/{}/chapters?{params}", manga.key);
					let response = web_view.build_request(&url)?.send()?;
					let response =
						web_view.decode_json_owned::<ChapterDetailsResponse>(&response)?;
					let last_page = response.result.meta.last_page;
					chapters.extend(response.result.items);
					if page >= last_page {
						break;
					}
					page += 1;
				}
				Ok(chapters)
			};
			let mut chapter_list = if has_signer {
				fetch_signed().or_else(|_| web_view.fetch_chapters(&page_url, &manga.key))?
			} else {
				web_view.fetch_chapters(&page_url, &manga.key)?
			};

			if deduplicate {
				for item in chapter_list.drain(..) {
					helpers::dedup_insert(&mut chapter_map, item);
				}
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

	fn get_page_list(&self, manga: Manga, chapter: Chapter) -> Result<Vec<Page>> {
		let mut web_view = self.web_view.borrow_mut();
		let signed_url = format!("{}/chapters/{}", settings::api_url(), chapter.key);
		let page_url = selected_source_url(
			chapter.url.as_deref(),
			&format!("title/{}/{}", manga.key, chapter.key),
		);
		let json = if web_view.has_signer() {
			web_view
				.build_request(&signed_url)
				.and_then(|request| Ok(request.send()?))
				.and_then(|response| web_view.decode_json_owned::<ChapterResponse>(&response))
				.or_else(|_| web_view.fetch_pages(&page_url))?
		} else {
			web_view.fetch_pages(&page_url)?
		};

		let Some(result) = json.result else {
			bail!("Missing chapter")
		};

		let base_url = result.pages.base_url.trim_end_matches('/');
		Ok(result
			.pages
			.items
			.into_iter()
			.enumerate()
			.map(|(index, page)| {
				let mut url = if page.url.starts_with("http") {
					page.url
				} else {
					format!("{base_url}/{}", page.url.trim_start_matches('/'))
				};
				let is_v3 = page.s == Some(1)
					|| url.split('?').skip(1).any(|query| {
						query
							.split('&')
							.any(|item| item == "v3" || item.starts_with("v3="))
					});
				if is_v3
					&& !url.split('?').skip(1).any(|q| {
						q.split('&')
							.any(|item| item == "v3" || item.starts_with("v3="))
					}) {
					url.push(if url.contains('?') { '&' } else { '?' });
					url.push_str("v3");
				}
				// Older chapter payloads may still mark every fourth image for the legacy
				// descrambler. Keep that context so encrypted cached responses remain decodable.
				let is_legacy_scramble = is_legacy_scramble_page(index, is_v3);
				let mut context = PageContext::new();
				context.insert("image_url".into(), url.clone());
				if is_v3 {
					context.insert("s".into(), "1".into());
				}
				if is_legacy_scramble {
					context.insert("legacy_scramble".into(), "1".into());
				}
				context.insert("width".into(), page.width.to_string());
				context.insert("height".into(), page.height.to_string());
				Page {
					content: PageContent::url_context(url, context),
					..Default::default()
				}
			})
			.collect())
	}
}

impl Home for Comix {
	fn get_home(&self) -> Result<HomeLayout> {
		let api_url = settings::api_url();
		// send basic layout
		send_partial_result(&HomePartialResult::Layout(HomeLayout {
			components: vec![
				HomeComponent {
					title: Some("Most Recent Popular".into()),
					subtitle: None,
					value: aidoku::HomeComponentValue::empty_scroller(),
				},
				HomeComponent {
					title: Some("Most Follows New Comics".into()),
					subtitle: None,
					value: aidoku::HomeComponentValue::empty_scroller(),
				},
				HomeComponent {
					title: Some("Latest Updates (Hot)".into()),
					subtitle: None,
					value: aidoku::HomeComponentValue::empty_scroller(),
				},
				HomeComponent {
					title: Some("Recently Added".into()),
					subtitle: None,
					value: aidoku::HomeComponentValue::empty_manga_chapter_list(),
				},
			],
		}));

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

		let mut web_view = self.web_view.borrow_mut();
		let base_url = settings::base_url();
		let popular_res = fetch_search_response(
			&mut web_view,
			&format!("{api_url}/manga/top?type=trending&days=1&limit=20{extra_qs}"),
			&format!("{base_url}/browse?order[views_7d]=desc&page=1{extra_qs}"),
		)?;
		let follows_res = fetch_search_response(
			&mut web_view,
			&format!("{api_url}/manga/top?type=follows&days=1&limit=20{extra_qs}"),
			&format!("{base_url}/browse?order[follows_total]=desc&page=1{extra_qs}"),
		)?;
		let latest_res = fetch_search_response(
			&mut web_view,
			&format!(
				"{api_url}/manga?scope=hot&limit=30&order[chapter_updated_at]=desc&page=1{extra_qs}"
			),
			&format!("{base_url}/browse?order[chapter_updated_at]=desc&page=1{extra_qs}"),
		)?;
		let recent_res = fetch_search_response(
			&mut web_view,
			&format!("{api_url}/manga?order[created_at]=desc&limit=10&page=1{extra_qs}"),
			&format!("{base_url}/browse?order[created_at]=desc&page=1{extra_qs}"),
		)?;

		for (response, title) in [
			(popular_res, "Most Recent Popular"),
			(follows_res, "Most Follows New Comics"),
			(latest_res, "Latest Updates (Hot)"),
		] {
			let entries = response
				.result
				.items
				.into_iter()
				.filter(|m| !m.is_hidden(&hidden_types, &hidden_terms))
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
				title: Some(title.into()),
				subtitle: None,
				value: aidoku::HomeComponentValue::Scroller {
					entries,
					listing: Some(Listing {
						id: title.into(),
						name: title.into(),
						..Default::default()
					}),
				},
			}));
		}

		{
			let entries = recent_res
				.result
				.items
				.into_iter()
				.filter(|m| !m.is_hidden(&hidden_types, &hidden_terms))
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
			let title = "Recently Added";
			send_partial_result(&HomePartialResult::Component(HomeComponent {
				title: Some(title.into()),
				subtitle: None,
				value: aidoku::HomeComponentValue::MangaChapterList {
					page_size: None,
					entries,
					listing: Some(Listing {
						id: title.into(),
						name: title.into(),
						..Default::default()
					}),
				},
			}));
		}

		Ok(HomeLayout::default())
	}
}

impl ListingProvider for Comix {
	fn get_manga_list(&self, listing: Listing, page: i32) -> Result<MangaPageResult> {
		let api_url = settings::api_url();
		let base_url = settings::base_url();
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

		fn get_listing_page(
			comix: &Comix,
			signed_url: &str,
			browse_url: &str,
		) -> Result<MangaPageResult> {
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
			let signed_url = format!("{signed_url}{extra_qs}");
			let browse_url = format!("{browse_url}{extra_qs}");
			let mut web_view = comix.web_view.borrow_mut();

			fetch_search_response(&mut web_view, &signed_url, &browse_url)
				.map(|r| r.result.into_filtered(&hidden_types, &hidden_terms))
		}

		match listing.id.as_str() {
			"Trending Webtoon" => trending(vec!["manhua".into(), "manhwa".into()]),
			"Trending Manga" => trending(vec!["manga".into()]),

			"Most Recent Popular" => get_listing_page(
				self,
				&format!("{api_url}/manga/top?type=trending&days=1&limit=50"),
				&format!("{base_url}/browse?order[views_7d]=desc&page={page}"),
			),
			"Most Follows New Comics" => get_listing_page(
				self,
				&format!("{api_url}/manga/top?type=follows&days=1&limit=50"),
				&format!("{base_url}/browse?order[follows_total]=desc&page={page}"),
			),

			"Latest Updates (Hot)" => get_listing_page(
				self,
				&format!(
					"{api_url}/manga?scope=hot&limit=30&order[chapter_updated_at]=desc&page={page}"
				),
				&format!("{base_url}/browse?order[chapter_updated_at]=desc&page={page}"),
			),
			"Recently Added" => get_listing_page(
				self,
				&format!("{api_url}/manga?order[created_at]=desc&limit=30&page={page}"),
				&format!("{base_url}/browse?order[created_at]=desc&page={page}"),
			),

			_ => bail!("Unknown listing"),
		}
	}
}

impl ImageRequestProvider for Comix {
	fn get_image_request(&self, url: String, _context: Option<PageContext>) -> Result<Request> {
		// Comix's image hosts reject requests carrying the site's Origin or Referer.
		// Keep ordinary cover and page requests headerless, matching the website's
		// referrer-policy="no-referrer" behavior.
		Ok(Request::get(&url)?)
	}
}

impl PageImageProcessor for Comix {
	fn process_page_image(
		&self,
		response: ImageResponse,
		context: Option<PageContext>,
	) -> Result<ImageRef> {
		let is_scrambled = context.as_ref().is_some_and(|context| {
			context.get("s").is_some_and(|value| value == "1")
				|| context
					.get("legacy_scramble")
					.is_some_and(|value| value == "1")
		});
		let original_url = response.request.url.clone().or_else(|| {
			context
				.as_ref()
				.and_then(|context| context.get("image_url"))
				.cloned()
		});

		if response.code < 400 {
			return Ok(finalize_page_image(response, is_scrambled));
		}

		// Comix rotates image path variants independently of the chapter payload. Only probe
		// alternate variants for a missing image. A Cloudflare 403 applies to every variant;
		// retrying it only creates a request storm and leaves the reader spinning.
		if response.code == 404
			&& let Some(original) = original_url.as_ref()
		{
			for candidate in image_path_fallbacks(original) {
				let Ok(retry) = page_image_request(&candidate).and_then(|r| Ok(r.send()?)) else {
					continue;
				};
				if let Some(image) = image_from_response(retry, &candidate, is_scrambled) {
					return Ok(image);
				}
			}
		}

		// The current CDN can reject URLSession while accepting a real browser image request.
		// Mirror the website by loading the same URL in WebKit, then return its rendered pixels.
		if let Some(url) = original_url.as_ref() {
			let data = ComixWebView::fetch_image(url)?;
			return Ok(ImageRef::new(&data));
		}

		bail!(
			"Comix's image CDN was blocked on this network. Try the other Website Domain in Source Settings, switch to cellular data, or use a VPN, then reload the chapter."
		)
	}
}

const IMAGE_RESPONSE_HEADERS: &[&str] = &[
	"x-enc-seed",
	"x-enc-len",
	"x-enc-algo",
	"x-scramble-grid",
	"x-scramble-algo",
	"x-scramble-seed",
	"x-scramble-hash",
];

fn page_image_request(url: &str) -> Result<Request> {
	// Match the website's referrer-policy="no-referrer" image requests. The CDN can still
	// reject CFNetwork clients, which process_page_image handles with its WebKit fallback.
	Ok(Request::get(url)?
		.header("Accept", "*/*")
		.header("User-Agent", IMAGE_USER_AGENT))
}

fn finalize_page_image(response: ImageResponse, is_scrambled: bool) -> ImageRef {
	if is_scrambled {
		decode_page_image(&response).unwrap_or(response.image)
	} else {
		response.image
	}
}

fn image_from_response(response: Response, url: &str, is_scrambled: bool) -> Option<ImageRef> {
	let code = response.status_code();
	if !(200..400).contains(&code) {
		return None;
	}
	let image = response.get_image().ok()?;
	if !is_scrambled {
		return Some(image);
	}

	let mut headers = HashMap::new();
	for &name in IMAGE_RESPONSE_HEADERS {
		if let Some(value) = response.get_header(name) {
			headers.insert(name.to_string(), value);
		}
	}
	Some(finalize_page_image(
		ImageResponse {
			code: code as u16,
			headers,
			request: ImageRequest {
				url: Some(url.to_string()),
				headers: HashMap::new(),
			},
			image,
		},
		true,
	))
}

const IMAGE_USER_AGENT: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) \
	AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Mobile/15E148 Safari/604.1";

const GRID_COLS: usize = 5;
const GRID_ROWS: usize = 5;
const GRID_TILES: usize = GRID_COLS * GRID_ROWS;
const ENC_MULTIPLIER: u32 = 1_000_005;
const ENC_INCREMENT: u32 = 1_234_567_891;
const LCG_MULTIPLIER: u32 = 1_664_525;
const LCG_INCREMENT: u32 = 1_013_904_223;

fn is_legacy_scramble_page(index: usize, is_v3: bool) -> bool {
	!is_v3 && (index + 1).is_multiple_of(4)
}

fn image_header<'a>(response: &'a ImageResponse, name: &str) -> Option<&'a str> {
	response
		.headers
		.iter()
		.find(|(key, _)| key.eq_ignore_ascii_case(name))
		.map(|(_, value)| value.as_str())
}

fn decode_page_image(response: &ImageResponse) -> Option<ImageRef> {
	let encoded_seed = image_header(response, "x-enc-seed")
		.and_then(|value| value.parse::<i64>().ok())
		.unwrap_or(0) as i32;
	let encoded_length =
		image_header(response, "x-enc-len").and_then(|value| value.parse::<usize>().ok());
	let encoded_algorithm = image_header(response, "x-enc-algo");
	let encoded = encoded_seed != 0 && encoded_length.is_some();

	let mut decoded_image = None;
	if encoded {
		let bytes = response.image.data();
		if bytes.is_empty() {
			return None;
		}
		let decoded = decode_encoded_bytes(
			bytes,
			encoded_seed,
			encoded_length.unwrap_or_default(),
			encoded_algorithm,
		);
		decoded_image = Some(ImageRef::new(&decoded));
	}

	let should_descramble = image_header(response, "x-scramble-grid") == Some("5x5");
	if !should_descramble {
		return decoded_image;
	}
	let algorithm = image_header(response, "x-scramble-algo");
	if !matches!(algorithm, None | Some("1" | "2" | "3")) {
		return decoded_image;
	}

	let seed = image_header(response, "x-scramble-seed")?
		.parse::<i64>()
		.ok()? as i32;
	if seed == 0 {
		return None;
	}
	let hash = match image_header(response, "x-scramble-hash").map(str::trim) {
		Some("03632") => 58_414,
		Some("02900") => 117_532,
		_ => 0,
	};
	let mixed_seed = seed ^ hash;
	let order = if algorithm == Some("3") {
		build_xorshift_order(mixed_seed, GRID_TILES)
	} else {
		build_lcg_order(mixed_seed, GRID_TILES)
	};

	let image = decoded_image.as_ref().unwrap_or(&response.image);
	let width = image.width();
	let height = image.height();
	let tile_width = (width as u32 / GRID_COLS as u32) as f32;
	let tile_height = (height as u32 / GRID_ROWS as u32) as f32;
	if tile_width == 0.0 || tile_height == 0.0 {
		return None;
	}

	let mut canvas = Canvas::new(width, height);
	// Preserve remainder pixels at the right and bottom when dimensions are not divisible by 5.
	canvas.draw_image(image, Rect::new(0.0, 0.0, width, height));
	for (destination, source) in order.into_iter().enumerate() {
		let source_x = (source % GRID_COLS) as f32 * tile_width;
		let source_y = (source / GRID_COLS) as f32 * tile_height;
		let destination_x = (destination % GRID_COLS) as f32 * tile_width;
		let destination_y = (destination / GRID_COLS) as f32 * tile_height;
		canvas.copy_image(
			image,
			Rect::new(source_x, source_y, tile_width, tile_height),
			Rect::new(destination_x, destination_y, tile_width, tile_height),
		);
	}
	Some(canvas.get_image())
}

fn decode_encoded_bytes(
	bytes: Vec<u8>,
	seed: i32,
	length: usize,
	algorithm: Option<&str>,
) -> Vec<u8> {
	if algorithm != Some("2") {
		return decode_with_lcg(bytes, seed, length);
	}

	let candidates = [
		decode_with_xorshift(bytes.clone(), seed | 1, length, false),
		decode_with_xorshift(bytes.clone(), seed, length, false),
		decode_with_xorshift(bytes.clone(), seed | 1, length, true),
		decode_with_lcg(bytes, seed, length),
	];
	candidates
		.iter()
		.find(|candidate| has_image_signature(candidate))
		.cloned()
		.unwrap_or_else(|| candidates[0].clone())
}

fn decode_with_xorshift(mut bytes: Vec<u8>, seed: i32, length: usize, high_byte: bool) -> Vec<u8> {
	let mut state = seed as u32;
	let limit = bytes.len().min(length);
	for byte in bytes.iter_mut().take(limit) {
		state ^= state.wrapping_shl(13);
		state ^= state.wrapping_shr(17);
		state ^= state.wrapping_shl(5);
		let key = if high_byte { state >> 24 } else { state & 0xff };
		*byte ^= key as u8;
	}
	bytes
}

fn decode_with_lcg(mut bytes: Vec<u8>, seed: i32, length: usize) -> Vec<u8> {
	let mut state = seed as u32;
	let limit = bytes.len().min(length);
	for byte in bytes.iter_mut().take(limit) {
		state = state
			.wrapping_mul(ENC_MULTIPLIER)
			.wrapping_add(ENC_INCREMENT);
		*byte ^= (state >> 24) as u8;
	}
	bytes
}

fn has_image_signature(bytes: &[u8]) -> bool {
	bytes.starts_with(&[0xff, 0xd8])
		|| bytes.starts_with(&[0x89, b'P', b'N', b'G'])
		|| (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
}

fn inverse_order(shuffled: Vec<usize>) -> Vec<usize> {
	let mut inverse = vec![0; shuffled.len()];
	for (index, value) in shuffled.into_iter().enumerate() {
		inverse[value] = index;
	}
	inverse
}

fn build_xorshift_order(seed: i32, count: usize) -> Vec<usize> {
	let mut shuffled: Vec<usize> = (0..count).collect();
	let mut state = seed as u32 | 1;
	for index in (1..count).rev() {
		state ^= state.wrapping_shl(13);
		state ^= state.wrapping_shr(17);
		state ^= state.wrapping_shl(5);
		let swap_index = state as usize % (index + 1);
		shuffled.swap(index, swap_index);
	}
	inverse_order(shuffled)
}

fn build_lcg_order(seed: i32, count: usize) -> Vec<usize> {
	let mut shuffled: Vec<usize> = (0..count).collect();
	let mut state = seed as u32;
	for index in (1..count).rev() {
		state = state
			.wrapping_mul(LCG_MULTIPLIER)
			.wrapping_add(LCG_INCREMENT);
		let swap_index = state as usize % (index + 1);
		shuffled.swap(index, swap_index);
	}
	inverse_order(shuffled)
}

/// Alternate CDN path segments used by current and older Comix chapter payloads.
const IMAGE_PATH_SEGMENTS: &[&str] = &["/fcf/", "/i5/", "/si/", "/i/", "/sii/", "/ii/", "/hi/"];

/// Returns fallback URLs by swapping the image path segment for each alternative.
fn image_path_fallbacks(url: &str) -> Vec<String> {
	let Some(current) = IMAGE_PATH_SEGMENTS.iter().find(|s| url.contains(**s)) else {
		return Vec::new();
	};
	IMAGE_PATH_SEGMENTS
		.iter()
		.filter(|s| *s != current)
		.map(|s| url.replacen(*current, s, 1))
		.collect()
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
		let Some(path) = ["https://comix.to/", "https://comix.ws/"]
			.into_iter()
			.find_map(|base| url.strip_prefix(base))
		else {
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

#[cfg(test)]
mod tests {
	use super::*;
	use aidoku_test::aidoku_test;

	#[aidoku_test]
	fn builds_xorshift_scramble_order() {
		assert_eq!(
			build_xorshift_order(123_456_789, 25),
			vec![
				16, 5, 22, 14, 7, 20, 24, 13, 21, 4, 15, 3, 23, 0, 11, 18, 12, 1, 10, 17, 8, 9, 6,
				19, 2,
			]
		);
	}

	#[aidoku_test]
	fn builds_lcg_scramble_order() {
		assert_eq!(
			build_lcg_order(123_456_789, 25),
			vec![
				2, 16, 6, 19, 14, 18, 12, 24, 10, 9, 8, 17, 0, 13, 3, 11, 1, 20, 4, 22, 7, 21, 5,
				23, 15,
			]
		);
	}

	#[aidoku_test]
	fn decodes_lcg_page_prefix() {
		let original = b"\xff\xd8comix-image-data".to_vec();
		let encoded = decode_with_lcg(original.clone(), 123_456, original.len());
		assert_eq!(decode_with_lcg(encoded, 123_456, original.len()), original);
	}

	#[aidoku_test]
	fn decodes_xorshift_page_prefix() {
		let original = b"RIFFxxxxWEBPcomix-image-data".to_vec();
		let encoded = decode_with_xorshift(original.clone(), 123_457, original.len(), false);
		assert_eq!(
			decode_with_xorshift(encoded, 123_457, original.len(), false),
			original
		);
	}

	#[aidoku_test]
	fn marks_only_legacy_fourth_pages() {
		assert!(!is_legacy_scramble_page(2, false));
		assert!(is_legacy_scramble_page(3, false));
		assert!(!is_legacy_scramble_page(3, true));
		assert!(is_legacy_scramble_page(7, false));
	}

	#[aidoku_test]
	fn retries_new_hi_image_paths() {
		let fallbacks = image_path_fallbacks("https://cdn.example/hi/page.webp?r=2");
		assert_eq!(
			fallbacks.first().map(String::as_str),
			Some("https://cdn.example/fcf/page.webp?r=2")
		);
		assert!(fallbacks.contains(&"https://cdn.example/i5/page.webp?r=2".into()));
		assert!(!fallbacks.iter().any(|url| url.contains("/hi/")));
	}
}
