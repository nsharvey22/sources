// reference: https://github.com/nobottomline/extensions-source/blob/c8fe930f315f3baee23587559edfceab5e969202/src/en/comix/src/eu/kanade/tachiyomi/extension/en/comix/Signer.kt
use crate::BASE_URL;
use aidoku::{
	Result,
	alloc::string::String,
	imports::{js::WebView, net::Request},
	prelude::*,
};
use regex::Regex;

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

const GET_VMOBJ_JS: &str = "\
const vmKey = Object.keys(window).find(key => key.startsWith('vm'));\
const vmObj = window[vmKey];\
if (!vmObj || typeof vmObj !== 'object' || vmObj === window) {\
	return '';\
}";

const JS_PATCHER: &str = "<head><script>window['originalGetImageData'] = HTMLCanvasElement.prototype.toDataURL;</script>";

pub struct ComixWebView {
	web_view: WebView,
	installer_fn: Option<String>,
	descrambler_fn: Option<String>,
}

pub fn create_web_view() -> Result<ComixWebView> {
	let web_view = WebView::new();
	web_view.load_html_blocking(
		Request::get(BASE_URL)?
			.string()?
			.replace("<head>", JS_PATCHER)
			.as_str(),
		Some(BASE_URL),
	)?;
	let mut comix_web_view = ComixWebView {
		web_view,
		installer_fn: None,
		descrambler_fn: None,
	};
	if find_functions(&mut comix_web_view).is_err() {
		find_secure_module_src(&mut comix_web_view)?;
		find_functions(&mut comix_web_view)?;
	}
	Ok(comix_web_view)
}

fn find_secure_module_src(web_view: &mut ComixWebView) -> Result<()> {
	let main_module_src = Request::get(BASE_URL)?
		.html()?
		.select("head > script[type=\"module\"][src*=\"main\"]")
		.and_then(|e| e.first())
		.and_then(|e| e.attr("src"))
		.ok_or(error!("Main module not found"))?;
	if let Some(js_asset_path_index) = main_module_src.rfind("/") {
		let js_asset_path = &main_module_src[0..js_asset_path_index + 1];
		let secure_script_regex = Regex::new("(secure-[A-Za-z0-9-_]+?\\.js)").unwrap();
		let main_module_contents =
			Request::get(format!("{BASE_URL}{main_module_src}"))?.string()?;
		if let Some(secure_script_path) = secure_script_regex
			.captures(main_module_contents.as_str())
			.and_then(|captures| captures.get(1).map(|m| m.as_str()))
		{
			web_view.web_view.eval(&format!(
				"(() => {{
				import('{BASE_URL}{js_asset_path}{secure_script_path}')
					.then((m) => window['vm'] = m)
					.catch((e) => window['vm'] = {{}});
				return '';
			}})()"
			))?;
			while web_view
				.web_view
				.eval("(() => { return window['vm'] == null ? 'true' : 'false'; })()")?
				== "true"
			{}
			Ok(())
		} else {
			bail!("Secure module not found");
		}
	} else {
		bail!("Invalid path")
	}
}

fn find_functions(web_view: &mut ComixWebView) -> Result<()> {
	let result = web_view.web_view.eval(&format!(
		"(() => {{
			try {{
				{GET_VMOBJ_JS}
				let fnames = Object.keys(vmObj);
				let inst = '', desc = '';
				const isPromise = (v) => v && (typeof v === 'object' || typeof v === 'function') && typeof v.then === 'function';
				const testCanvas = document.createElement('canvas');
				for (let j = 0; j < fnames.length; j++) {{
					let fn = vmObj[fnames[j]];
					if (typeof fn !== 'function') continue;
					let ref = 'window[' + JSON.stringify(vmKey) + '].' + fnames[j];
					if (!inst) {{
						try {{
							let got = false;
							fn({{
								interceptors: {{
									request: {{ use: function() {{}} }},
									response: {{ use: function() {{ got = true; }} }}
								}},
								defaults: {{
									headers: {{ common: {{}} }},
									transformRequest: [],
									transformResponse: []
								}}
							}});
							if (got) inst = ref;
						}} catch (e) {{}}
					}}
					if (!desc) {{
						try {{
							if (fn.constructor && fn.constructor.name === 'AsyncFunction') {{
								desc = ref;
							}} else if (fn.length >= 2) {{
								let res = fn('about:blank', testCanvas);
								if (isPromise(res)) desc = ref;
							}}
						}} catch (e) {{}}
					}}
				}}
				return inst + '||' + desc
			}} catch(e) {{}}
			return '';
		}})()",
	))?;
	let Some((installer_expr, descrambler_expr)) = result.split_once("||") else {
		bail!("Failed to find installer and descrambler functions")
	};
	if installer_expr.is_empty() {
		bail!("Failed to find installer function");
	};
	if descrambler_expr.is_empty() {
		bail!("Failed to find descrambler function");
	};
	web_view.installer_fn = Some(installer_expr.into());
	web_view.descrambler_fn = Some(descrambler_expr.into());
	Ok(())
}

#[allow(dead_code)]
/// * `path`: API path, e.g. "/manga/some-hash/chapters"
pub fn get_token(web_view: &ComixWebView, path: &str) -> Result<String> {
	let Some(installer_fn) = web_view.installer_fn.as_ref() else {
		bail!("Missing installer function")
	};
	let token = web_view.web_view.eval(&format!(
		"(() => {{
			try {{
				{GET_VMOBJ_JS}
				let captured = {{ req: null, res: null }};
				{installer_fn}({{
					interceptors: {{
						request: {{
							use: function (fn) {{ captured.req = fn; }},
						}},
						response: {{
							use: function (fn) {{ captured.res = fn; }},
						}},
					}},
					defaults: {{
						headers: {{ common: {{}} }},
						transformRequest: [],
						transformResponse: []
					}}
				}});
				return captured.req({{ url: '{path}', method: 'GET' }}).params['_'];
			}} catch(e) {{
				return '';
			}}
		}})()"
	))?;
	if token.is_empty() {
		bail!("Failed to fetch token")
	}
	Ok(token)
}

#[allow(dead_code)]
pub fn decode_response(web_view: &ComixWebView, url: &str, encoded_res: &str) -> Result<String> {
	let Some(installer_fn) = web_view.installer_fn.as_ref() else {
		bail!("Missing installer function")
	};

	let json = serde_json::from_str::<serde_json::Value>(encoded_res)
		.map_err(|_| error!("Invalid api response"))?;
	let is_encoded = match json {
		serde_json::Value::Object(ref map) => map.contains_key("e"),
		_ => false,
	};
	if !is_encoded {
		return Ok(encoded_res.into());
	};

	let encoded_res_escaped = encoded_res.replace("'", "\\'");
	let result = web_view.web_view.eval(&format!(
		"(() => {{
			try {{
				{GET_VMOBJ_JS}
				let captured = {{ req: null, res: null }};
				{installer_fn}({{
					interceptors: {{
						request: {{
							use: function (fn) {{ captured.req = fn; }},
						}},
						response: {{
							use: function (fn) {{ captured.res = fn; }},
						}},
					}},
					defaults: {{
						headers: {{ common: {{}} }},
						transformRequest: [],
						transformResponse: []
					}}
				}});
				if (!captured.res) {{
					return 'error: could not capture response handler';
				}}

				let raw = JSON.parse('{encoded_res_escaped}');
				let fakeResp = {{
					data: raw,
					status: 200,
					statusText: '',
					headers: {{
						'x-enc': '1',
					}},
					config: {{ url: '{url}', method: 'get', baseURL: '/api/v1' }},
					request: {{}},
				}};
				let decoded = captured.res(fakeResp);
				return JSON.stringify({{ result: decoded && decoded.data }});
			}} catch(e) {{
				return 'error: ' + e;
			}}
		}})()",
	))?;
	if result.starts_with("error:") {
		bail!("{result}");
	} else if result.is_empty() {
		bail!("Failed to fetch token")
	}
	Ok(result)
}

pub fn descramble_image(
	web_view: &ComixWebView,
	width: f32,
	height: f32,
	url: &str,
) -> Result<String> {
	let Some(descrambler_fn) = web_view.descrambler_fn.as_ref() else {
		bail!("Missing descramble function")
	};

	web_view.web_view.eval(&format!(
		"(() => {{
		const canvas = document.createElement('canvas');
		canvas.width = {width};
		canvas.height = {height};

		window['TEMP_CANVAS'] = canvas;
		window['TEMP_STATE'] = {{ isDone: false, error: null }}

		const controller = new AbortController();
		const signal = controller.signal;

		{GET_VMOBJ_JS}
		{descrambler_fn}('{url}', signal)
			.then((data) => {{
				const url = URL.createObjectURL(data);
				const image = new Image();
				image.src = url;
				image.onload = () => {{
					URL.revokeObjectURL(url);
					const ctx = canvas.getContext('2d');
					ctx.drawImage(image, 0, 0);
					window['TEMP_STATE'].isDone = true;
				}}
			}})
			.catch((e) => {{ window['TEMP_STATE'].isDone = true; window['TEMP_STATE'].error = e }});

		return '';
	}})()"
	))?;

	while web_view
		.web_view
		.eval("(() => { return window['TEMP_STATE'].isDone ? 'true' : 'false'; })()")?
		== "false"
	{}

	let result = web_view.web_view.eval(
		"(() => {{
		if (window['TEMP_STATE'].error) return '';
		const data = window['originalGetImageData'].call(window['TEMP_CANVAS']);
		return data;
	}})()",
	)?;

	if result.is_empty() {
		bail!("Failed to descramble image")
	} else {
		Ok(result)
	}
}
