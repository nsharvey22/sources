#![no_std]
extern crate alloc;

use core::cell::RefCell;

use aidoku::imports::canvas::{Canvas, ImageRef};
use aidoku::{
	Chapter, DeepLinkHandler, DeepLinkResult, FilterValue, HashMap, Home, HomeComponent,
	HomeLayout, HomePartialResult, ImageRequestProvider, ImageResponse, Link, LinkValue, Listing,
	ListingProvider, Manga, MangaPageResult, MangaWithChapter, NotificationHandler, Page,
	PageContent, PageContext, PageImageProcessor, Result, Source,
	alloc::{String, Vec, string::ToString, vec},
	helpers::uri::{QueryParameters, encode_uri_component},
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
const API_URL: &str = "https://comix.to/api/v1";

const CONTENT_TYPES: &[&str] = &["manga", "manhwa", "manhua", "other"];

struct Comix {
	// Lazily-created WebView reused to run the site's own JS descrambler for image
	// versions we can't descramble natively (see process_page_image). Cached because
	// loading the page + secure module is expensive.
	descramble_web_view: RefCell<Option<web::ComixWebView>>,
}

impl Source for Comix {
	fn new() -> Self {
		Self {
			descramble_web_view: RefCell::new(None),
		}
	}

	fn get_search_manga_list(
		&self,
		query: Option<String>,
		page: i32,
		filters: Vec<FilterValue>,
	) -> Result<MangaPageResult> {
		let web_view = web::create_web_view()?;

		let mut hidden_types = {
			let types = settings::hidden_types();
			if types.is_empty() { None } else { Some(types) }
		};
		let mut hidden_terms = settings::hidden_terms();

		// Collect non-sort filters to push after order/page/limit/content_rating
		// (param order must match real site: order → page → limit → content_rating → filters)
		let mut sort_key: Option<String> = None;
		let mut sort_val: Option<&'static str> = None;
		let mut text_filters: Vec<(String, String)> = Vec::new();  // (param_key, term_id)
		let mut select_filters: Vec<(String, String)> = Vec::new();
		let mut multi_included: Vec<(String, String)> = Vec::new();
		let mut multi_excluded: Vec<(String, String)> = Vec::new();

		for filter in &filters {
			match filter {
				FilterValue::Sort { id, index, ascending } => {
					sort_key = Some(format!(
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
					));
					sort_val = Some(if (*index == 3 && !ascending) || (*index != 3 && *ascending) {
						"asc"
					} else {
						"desc"
					});
				}
				FilterValue::MultiSelect { id, included, excluded } => {
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
							multi_included.push(("genres_in[]".into(), value.clone()));
						} else {
							multi_included.push((id.clone(), value.clone()));
						}
					}
					for value in excluded {
						if id == "genres[]" {
							if hidden_terms.contains(&value.parse().unwrap_or_default()) {
								continue;
							}
							multi_excluded.push(("genres_ex[]".into(), value.clone()));
						} else {
							multi_excluded.push((id.clone(), format!("-{value}")));
						}
					}
				}
				FilterValue::Select { id, value } => {
					select_filters.push((id.clone(), value.clone()));
				}
				FilterValue::Text { id, value } => {
					text_filters.push((id.clone(), value.clone()));
				}
				_ => {}
			}
		}

		// Resolve text filters (term lookups) before building the URL
		let mut resolved_text: Vec<(String, String)> = Vec::new();
		for (id, value) in text_filters {
			let url = format!(
				"{API_URL}/terms?type={id}&keyword={}&limit=1",
				encode_uri_component(value)
			);
			println!("[comix] search terms url: {url}");
			let raw = web::fetch_api(&web_view, &url)?;
			let term_id = serde_json::from_str::<TermResponse>(&raw)
				.map_err(|e| error!("{e}"))?
				.result
				.items
				.first()
				.map(|t| t.id)
				.ok_or_else(|| error!("No matching {id}s"))?;
			resolved_text.push((format!("{id}s[]"), term_id.to_string()));
		}

		// Build params in real-site order: order → page → limit → content_rating → filters
		let mut qs = QueryParameters::new();

		if let (Some(key), Some(val)) = (sort_key, sort_val) {
			qs.push(&key, Some(val));
		} else {
			qs.push("order[relevance]", Some("desc"));
		}

		qs.push("page", Some(&page.to_string()));
		qs.push("limit", Some("28"));
		for rating in settings::content_ratings() {
			qs.push("content_rating[]", Some(rating));
		}

		if query.is_some() {
			qs.push("keyword", query.as_deref());
		}

		for (key, val) in resolved_text {
			qs.push(&key, Some(&val));
		}
		for (key, val) in select_filters {
			qs.push(&key, Some(&val));
		}
		for (key, val) in multi_included {
			qs.push(&key, Some(&val));
		}
		for (key, val) in multi_excluded {
			qs.push(&key, Some(&val));
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

		let url = format!("{API_URL}/manga?{qs}");
		println!("[comix] search url: {url}");
		let raw = web::fetch_api(&web_view, &url)?;
		println!("[comix] search raw ({} bytes): {}", raw.len(), &raw[..raw.len().min(300)]);
		serde_json::from_str::<SearchResponse>(&raw)
			.map(Into::into)
			.map_err(|e| { println!("[comix] search parse error: {e}"); error!("{e}") })
	}

	fn get_manga_update(
		&self,
		mut manga: Manga,
		needs_details: bool,
		needs_chapters: bool,
	) -> Result<Manga> {
		if needs_details {
			let url = format!("{API_URL}/manga/{}", manga.key);
			println!("[comix] manga detail url: {url}");
			let web_view = web::create_web_view()?;
			let raw = web::fetch_api(&web_view, &url)?;
			println!("[comix] manga detail raw ({} bytes): {}", raw.len(), &raw[..raw.len().min(300)]);
			let json: SingleMangaResponse = serde_json::from_str(&raw)
				.map_err(|e| { println!("[comix] manga detail parse error: {e}"); error!("{e}") })?;
			manga.copy_from(json.result.into());
			if needs_chapters {
				send_partial_result(&manga);
			}
		}

		if needs_chapters {
			let limit = 100;
			let mut page = 1;
			let deduplicate = settings::dedupchapter();
			let mut chapter_map: HashMap<String, ComixChapter> = HashMap::new();
			let mut chapter_list: Vec<ComixChapter> = Vec::new();

			let web_view = web::create_web_view()?;
			let path = format!("/manga/{}/chapters", manga.key);

			loop {
				let url = format!(
					"{API_URL}{path}?limit={limit}&page={page}&order[number]=desc"
				);
				println!("[comix] chapters url (page {page}): {url}");

				let encoded_res = web::fetch_api(&web_view, &url)?;
				println!("[comix] chapters encoded ({} bytes): {}", encoded_res.len(), &encoded_res[..encoded_res.len().min(200)]);
				let result = web::decode_response(&web_view, &url, &encoded_res)?;
				println!("[comix] chapters decoded ({} bytes): {}", result.len(), &result[..result.len().min(200)]);
				let res = serde_json::from_str::<ChapterDetailsResponse>(&result)
					.map_err(|e| { println!("[comix] chapters parse error: {e}"); error!("{e}") })?;

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
		println!("[comix] get_page_list: chapter.key={}", chapter.key);
		let web_view = web::create_web_view()?;
		let path = format!("/chapters/{}", chapter.key);
		let url = format!("{API_URL}{path}");
		println!("[comix] page_list url: {url}");
		let encoded_res = web::fetch_api(&web_view, &url)?;
		println!("[comix] page_list encoded_res ({} bytes): {}", encoded_res.len(), &encoded_res[..encoded_res.len().min(200)]);
		let result = web::decode_response(&web_view, &url, &encoded_res)?;
		println!("[comix] page_list decoded ({} bytes): {}", result.len(), &result[..result.len().min(200)]);
		let json: ChapterResponse = serde_json::from_str(&result)
			.map_err(|e| { println!("[comix] page_list parse error: {e}"); error!("{e}") })?;

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
					content: {
						let mut context = PageContext::new();
						// Use the tile-scramble seed if present, otherwise "1" as a
						// non-zero sentinel. Any non-"0" value causes process_page_image
						// to re-fetch the URL and inspect response headers, which is the
						// only way to discover XOR encoding (x-enc-seed/x-enc-algo) for
						// pages that have no tile scrambling (page.s == None or 0).
						// Pages without XOR encoding return response.image immediately after
						// the re-fetch, so the extra round-trip is cheap (usually a cache hit).
						let s_val = page.s.unwrap_or(0);
						context.insert("s".into(), if s_val != 0 { s_val.to_string() } else { "1".into() });
						// Only include dimensions when the API provided non-zero values.
						// If width or height is 0, omitting them prevents the reader from
						// computing a NaN cell height via containerWidth * 0 / 0.
						if page.width > 0.0 && page.height > 0.0 {
							context.insert("width".into(), page.width.to_string());
							context.insert("height".into(), page.height.to_string());
						}
						PageContent::url_context(url, context)
					},
					..Default::default()
				}
			})
			.collect())
	}
}

impl Home for Comix {
	fn get_home(&self) -> Result<HomeLayout> {
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

		let cr = settings::content_rating_qs();
		let hidden_types = settings::hidden_types();
		let hidden_terms = settings::hidden_terms();

		let web_view = match web::create_web_view() {
			Ok(wv) => wv,
			Err(e) => {
				println!("[comix] home create_web_view failed: {e:?}");
				return Ok(HomeLayout::default());
			}
		};

		let home_sections = [
			("Most Recent Popular",    format!("{API_URL}/manga/top?type=trending&days=1&limit=50&{cr}"), false),
			("Most Follows New Comics",format!("{API_URL}/manga/top?type=follows&days=1&limit=50&{cr}"),  false),
			("Latest Updates (Hot)",   format!("{API_URL}/manga?order[chapter_updated_at]=desc&scope=hot&{cr}&page=1&limit=31"), false),
			("Recently Added",         format!("{API_URL}/manga?order[created_at]=desc&{cr}&page=1&limit=31"), true),
		];

		for (title, url, is_chapter_list) in &home_sections {
			println!("[comix] home '{title}' url: {url}");
			let raw = match web::fetch_api(&web_view, url) {
				Ok(s) => s,
				Err(e) => {
					println!("[comix] home '{title}' request error: {e:?}");
					continue;
				}
			};
			println!("[comix] home '{title}' raw ({} bytes): {}", raw.len(), &raw[..raw.len().min(300)]);
			let search: SearchResponse = match serde_json::from_str(&raw) {
				Ok(s) => s,
				Err(e) => {
					println!("[comix] home '{title}' parse error: {e}");
					continue;
				}
			};

			if *is_chapter_list {
				let entries = search
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
				let entries = search
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

		fn get_listing_page(url: &str) -> Result<MangaPageResult> {
			let hidden_types = settings::hidden_types();
			let hidden_terms = settings::hidden_terms();
			println!("[comix] listing url: {url}");
			let web_view = web::create_web_view()?;
			let raw = web::fetch_api(&web_view, url)?;
			println!("[comix] listing raw ({} bytes): {}", raw.len(), &raw[..raw.len().min(300)]);
			serde_json::from_str::<SearchResponse>(&raw)
				.map(|r| r.result.into_filtered(&hidden_types, &hidden_terms))
				.map_err(|e| { println!("[comix] listing parse error: {e}"); error!("{e}") })
		}

		let cr = settings::content_rating_qs();

		match listing.id.as_str() {
			"Trending Webtoon" => trending(vec!["manhua".into(), "manhwa".into()]),
			"Trending Manga" => trending(vec!["manga".into()]),

			"Most Recent Popular" => get_listing_page(&format!(
				"{API_URL}/manga/top?type=trending&days=1&limit=50&{cr}"
			)),
			"Most Follows New Comics" => get_listing_page(&format!(
				"{API_URL}/manga/top?type=follows&days=1&limit=50&{cr}"
			)),

			"Latest Updates (Hot)" => get_listing_page(&format!(
				"{API_URL}/manga?order[chapter_updated_at]=desc&scope=hot&{cr}&page={page}&limit=31"
			)),
			"Recently Added" => get_listing_page(&format!(
				"{API_URL}/manga?order[created_at]=desc&{cr}&page={page}&limit=31"
			)),

			_ => bail!("Unknown listing"),
		}
	}
}

impl ImageRequestProvider for Comix {
	fn get_image_request(&self, url: String, _context: Option<PageContext>) -> Result<Request> {
		Ok(Request::get(url)?.header("Referer", &format!("{BASE_URL}/")))
	}
}

impl Comix {
	/// Descramble an image using the site's own JS descrambler (the `ns`/`__comixDesc`
	/// function from secure-*.js), run in a cached WebView. This is the fallback for
	/// scrambled images whose x-scramble-hash version we have no native seed correction
	/// for — the JS handles XOR + tile descramble for any version. Returns the final image.
	fn descramble_via_web_view(&self, url: &str, width: f32, height: f32) -> Result<ImageRef> {
		// lazily create + cache the web view (loading the page + secure module is expensive)
		if self.descramble_web_view.borrow().is_none() {
			let wv = web::create_web_view()?;
			*self.descramble_web_view.borrow_mut() = Some(wv);
		}

		// try the cached web view; if it errors (stale clearance / module), rebuild once
		let data_url = {
			let slot = self.descramble_web_view.borrow();
			web::descramble_image(slot.as_ref().unwrap(), width, height, url)
		};
		let data_url = match data_url {
			Ok(d) => d,
			Err(e) => {
				println!("[comix] descramble_via_web_view: retrying with fresh web view ({e:?})");
				let wv = web::create_web_view()?;
				*self.descramble_web_view.borrow_mut() = Some(wv);
				let slot = self.descramble_web_view.borrow();
				web::descramble_image(slot.as_ref().unwrap(), width, height, url)?
			}
		};

		let bytes = decode_data_url(&data_url)?;
		Ok(ImageRef::new(&bytes))
	}
}

/// Decode a `data:` URL (e.g. `data:image/png;base64,...`) into raw image bytes.
fn decode_data_url(data_url: &str) -> Result<Vec<u8>> {
	use base64::Engine;
	let comma = data_url.find(',').ok_or(error!("Invalid data URL"))?;
	base64::engine::general_purpose::STANDARD
		.decode(data_url[comma + 1..].as_bytes())
		.map_err(|e| error!("base64 decode failed: {e}"))
}

impl PageImageProcessor for Comix {
	fn process_page_image(
		&self,
		response: ImageResponse,
		context: Option<PageContext>,
	) -> Result<ImageRef> {
		let is_scrambled = context
			.as_ref()
			.and_then(|c| c.get("s"))
			.is_some_and(|s| s != "0");
		if !is_scrambled {
			println!("[comix] process_page_image: not scrambled, returning response.image");
			return Ok(response.image);
		}

		let url = response.request.url.ok_or(error!("Missing image URL"))?;

		// response.headers from ImageResponse is always empty in the Aidoku framework.
		// Re-fetch the image with send() to get a Response whose get_header() works.
		let resp = Request::get(&url)?
			.header("Referer", &format!("{BASE_URL}/"))
			.send()
			.map_err(|e| error!("{e:?}"))?;

		let enc_seed   = resp.get_header("x-enc-seed").and_then(|v| v.parse::<i64>().ok()).map(|v| v as i32);
		let enc_len    = resp.get_header("x-enc-len").and_then(|v| v.parse::<usize>().ok());
		let enc_algo   = resp.get_header("x-enc-algo");
		let scr_seed   = resp.get_header("x-scramble-seed").and_then(|v| v.parse::<i64>().ok()).map(|v| v as i32);
		let scr_grid   = resp.get_header("x-scramble-grid");
		let scr_algo   = resp.get_header("x-scramble-algo");
		let scr_hash   = resp.get_header("x-scramble-hash");
		let correction = descramble::scramble_hash_correction(scr_hash.as_deref());

		println!("[comix] process_page_image: url={url} enc_seed={enc_seed:?} enc_len={enc_len:?} enc_algo={enc_algo:?} scr_seed={scr_seed:?} scr_grid={scr_grid:?} scr_algo={scr_algo:?} scr_hash={scr_hash:?} correction={correction:?}");

		// Resolve the true scramble seed for the native descrambler.
		//
		// The API page list embeds the unobfuscated seed in context["s"] (page.s). The
		// x-scramble-seed header carries that same seed XORed with a per-version constant
		// keyed by x-scramble-hash (see descramble::scramble_hash_correction) — i.e.
		// header_seed ^ K == page.s. We PREFER the page-list seed (no constant needed), and
		// otherwise use the header seed corrected by K. When we have neither — no page.s and
		// an unknown hash version (no K) — the native descrambler can't run, and we fall back
		// to the site's own JS descrambler further down.
		//
		// CDNs also sometimes strip the custom x-scramble-* headers from cached responses;
		// scr_algo/scr_grid below recover from the URL suffix / a 5x5 assumption.
		let context_seed = context.as_ref()
			.and_then(|c| c.get("s"))
			.and_then(|s| s.parse::<i64>().ok())
			.filter(|&v| v != 0 && v != 1)
			.map(|v| v as i32);
		let header_seed = scr_seed; // raw x-scramble-seed (obfuscated)
		let native_seed = context_seed.or_else(|| {
			match (header_seed, correction) {
				(Some(h), Some(k)) => Some(h ^ k),
				_ => None,
			}
		});
		let scr_algo = scr_algo.or_else(|| {
			// Extract ?vN as the first query parameter of the URL.
			url.split_once('?')
				.and_then(|(_, q)| {
					let first = q.split('&').next().unwrap_or(q);
					match first { "v1" | "v2" | "v3" => Some(first[1..].to_string()), _ => None }
				})
		});
		let has_seed = header_seed.is_some_and(|s| s != 0) || context_seed.is_some();
		let scr_grid = if scr_grid.is_none() && has_seed && scr_algo.is_some() {
			Some("5x5".into())
		} else {
			scr_grid
		};

		let needs_xor = enc_seed.is_some_and(|s| s != 0) && enc_len.is_some();
		let should_descramble = scr_grid.as_deref() == Some("5x5")
			&& has_seed
			&& matches!(scr_algo.as_deref(), None | Some("1") | Some("2") | Some("3"));

		println!("[comix] process_page_image: FINAL native_seed={native_seed:?} (context_seed={context_seed:?}, header_seed={header_seed:?}) scr_grid={scr_grid:?} scr_algo={scr_algo:?} should_descramble={should_descramble} needs_xor={needs_xor}");

		if !needs_xor && !should_descramble {
			println!("[comix] process_page_image: no decode needed, returning response.image");
			return Ok(response.image);
		}

		// If the image is scrambled but we can't recover the true seed natively (unknown
		// x-scramble-hash version with no page.s), fall back to the site's own JS
		// descrambler, which handles the XOR + tile descramble internally for any version.
		if should_descramble && native_seed.is_none() {
			println!("[comix] process_page_image: unknown hash {scr_hash:?}, using JS descrambler fallback");
			// canvas dimensions for the JS descrambler: prefer the page-list dims, else read
			// them from the (scrambled but structurally valid) re-fetched image.
			let ctx_w = context.as_ref().and_then(|c| c.get("width")).and_then(|s| s.parse::<f32>().ok());
			let ctx_h = context.as_ref().and_then(|c| c.get("height")).and_then(|s| s.parse::<f32>().ok());
			let (w, h) = if let (Some(w), Some(h)) = (ctx_w, ctx_h) {
				(w, h)
			} else {
				let raw = resp.get_data().map_err(|e| error!("{e:?}"))?;
				let img = ImageRef::new(&raw);
				(img.width(), img.height())
			};
			if w <= 0.0 || h <= 0.0 || w.is_nan() || h.is_nan() || w.is_infinite() || h.is_infinite() {
				println!("[comix] process_page_image: invalid fallback dims ({w}x{h}), using placeholder");
				return Ok(Canvas::new(1.0, 1.0).get_image());
			}
			return self.descramble_via_web_view(&url, w, h);
		}

		// Always use freshly re-fetched bytes for any image that needs processing.
		// Using response.image would cause double-descrambling on the second load:
		// Aidoku caches our processed output and passes it back as response.image,
		// so we'd scramble an already-descrambled image into garbage.
		let image = if needs_xor || should_descramble {
			let raw = resp.get_data().map_err(|e| error!("{e:?}"))?;
			let decoded = if needs_xor {
				descramble::decode_xor(
					&raw,
					enc_seed.unwrap(),
					enc_len.unwrap(),
					enc_algo.as_deref(),
				)
			} else {
				raw
			};
			let img = ImageRef::new(&decoded);
			let w = img.width();
			let h = img.height();
			println!("[comix] process_page_image: image dims w={w} h={h}");
			if w.is_nan() || h.is_nan() || w.is_infinite() || h.is_infinite() || w <= 0.0 || h <= 0.0 {
				println!("[comix] process_page_image: invalid image dims ({w}x{h}), using placeholder");
				return Ok(Canvas::new(1.0, 1.0).get_image());
			}
			img
		} else {
			response.image
		};

		if should_descramble {
			// native_seed is the true (unobfuscated) seed: either the page-list seed
			// or the hash-corrected header seed (see seed resolution above).
			Ok(descramble::descramble_tiles(
				&image,
				native_seed.unwrap(),
				scr_algo.as_deref(),
			))
		} else {
			Ok(image)
		}
	}
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
