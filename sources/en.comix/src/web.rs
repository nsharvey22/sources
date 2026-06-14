use crate::BASE_URL;
use aidoku::{
	Result,
	alloc::string::String,
	imports::{js::WebView, net::Request},
	prelude::*,
};

// These scripts are injected directly into the page <head> before any other script
// runs, so the JSON.parse proxy is active before the framework makes its API calls.

// Captures any parsed object with result.pages into window.__comixPageData.
const PAGE_PROXY_INLINE: &str = "\
<script>\
(function(){\
if(window.__comixPageData)return;\
var _o=JSON.parse;\
JSON.parse=function(){\
var r=_o.apply(this,arguments);\
try{if(r&&r.result&&r.result.pages)window.__comixPageData=JSON.stringify(r);}catch(e){}\
return r;};\
})();\
</script>";

// Captures any parsed object with result.items shaped like a chapter list.
const CHAPTER_PROXY_INLINE: &str = "\
<script>\
(function(){\
if(window.__comixChapterData)return;\
var _o=JSON.parse;\
JSON.parse=function(){\
var r=_o.apply(this,arguments);\
try{\
var it=r&&r.result&&r.result.items;\
if(Array.isArray(it)&&it.length>0&&it[0].id!==undefined&&it[0].number!==undefined)\
window.__comixChapterData=JSON.stringify(r);\
}catch(e){}\
return r;};\
})();\
</script>";

// Captures any result.items array with content.
const MANGA_LIST_PROXY_INLINE: &str = "\
<script>\
(function(){\
if(window.__comixMangaListData)return;\
var _o=JSON.parse;\
JSON.parse=function(){\
var r=_o.apply(this,arguments);\
try{\
if(r&&r.result&&Array.isArray(r.result.items)&&r.result.items.length>0)\
window.__comixMangaListData=JSON.stringify(r);\
}catch(e){}\
return r;};\
})();\
</script>";


/// Fetches the HTML for `page_url`, injects a JSON.parse proxy script into `<head>`,
/// loads it into a WebView, then polls until the proxy captures data or a JS timeout fires.
fn load_and_capture(page_url: &str, proxy_script: &str, capture_key: &str) -> Result<String> {
	println!("[comix] load_and_capture: fetching HTML for {page_url}");
	let html = Request::get(page_url)?
		.header(
			"Accept",
			"text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
		)
		.header("Accept-Language", "en-US,en;q=0.9")
		.header("Referer", &format!("{BASE_URL}/"))
		.string()?;
	println!("[comix] load_and_capture: got {} bytes, injecting proxy into <head>", html.len());

	let patched = html.replacen("<head>", &format!("<head>{proxy_script}"), 1);

	let web_view = WebView::new();
	web_view.load_html_blocking(patched.as_str(), Some(page_url))?;
	println!("[comix] load_and_capture: WebView loaded, polling for {capture_key}");

	// Set a JS-side 15-second timeout so this loop always exits regardless of whether
	// the proxy fires. Each eval() yields to the JS event loop, so setTimeout callbacks
	// can fire between iterations.
	web_view.eval(
		"setTimeout(function(){window['__comixTimeout']=true;},15000);''"
	)?;

	let check = format!(
		"(function(){{\
			var d=window['{capture_key}'];\
			if(d)return d;\
			if(window['__comixTimeout'])return '__TIMEOUT__';\
			return '';\
		}})()"
	);

	loop {
		let result = web_view.eval(&check)?;
		match result.as_str() {
			"__TIMEOUT__" => {
				bail!("Timeout: {capture_key} not captured within 15s for {page_url}")
			}
			"" | "null" | "undefined" => continue,
			_ => {
				println!(
					"[comix] load_and_capture: captured {} bytes for {capture_key}",
					result.len()
				);
				return Ok(result);
			}
		}
	}
}

pub fn fetch_page_list_data(chapter_url: &str) -> Result<String> {
	println!("[comix] fetch_page_list_data: {chapter_url}");
	load_and_capture(chapter_url, PAGE_PROXY_INLINE, "__comixPageData")
}

pub fn fetch_chapter_data(manga_page_url: &str) -> Result<String> {
	println!("[comix] fetch_chapter_data: {manga_page_url}");
	load_and_capture(manga_page_url, CHAPTER_PROXY_INLINE, "__comixChapterData")
}

pub fn fetch_manga_list_data(browse_url: &str) -> Result<String> {
	println!("[comix] fetch_manga_list_data: {browse_url}");
	load_and_capture(browse_url, MANGA_LIST_PROXY_INLINE, "__comixMangaListData")
}

pub fn fetch_manga_detail_data(title_url: &str) -> Result<String> {
	let html = Request::get(title_url)?
		.header(
			"Accept",
			"text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
		)
		.header("Accept-Language", "en-US,en;q=0.9")
		.header("Referer", &format!("{BASE_URL}/"))
		.string()?;

	// comix.to embeds all page data in <script id="initial-data"> as JSON.
	// The queries object holds React Query cache entries; the detail entry key
	// is a serialized JSON array like ["detail","<hid>"] so it contains "detail".
	let json_text = extract_initial_data(&html)
		.ok_or(error!("No initial-data script on {title_url}"))?;

	let root: serde_json::Value =
		serde_json::from_str(json_text).map_err(|e| error!("initial-data parse failed: {e}"))?;

	let queries = root["queries"]
		.as_object()
		.ok_or(error!("No queries in initial-data for {title_url}"))?;

	let detail = queries
		.iter()
		.find(|(k, _)| k.contains("\"detail\""))
		.map(|(_, v)| v)
		.ok_or(error!("No detail query in initial-data for {title_url}"))?;

	// The query value is the bare ComixManga; wrap to match SingleMangaResponse.
	Ok(format!("{{\"result\":{detail}}}"))
}

fn extract_initial_data(html: &str) -> Option<&str> {
	let id_pos = html.find("id=\"initial-data\"")?;
	let tag_end = id_pos + html[id_pos..].find('>')? + 1;
	let close = html[tag_end..].find("</script>")?;
	Some(html[tag_end..tag_end + close].trim())
}


