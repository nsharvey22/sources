// reference: https://github.com/nobottomline/extensions-source/blob/c8fe930f315f3baee23587559edfceab5e969202/src/en/comix/src/eu/kanade/tachiyomi/extension/en/comix/Signer.kt
use crate::{
	helpers::create_request_get,
	models::{ChapterResponse, ComixChapter, ComixManga, ErrorResponse, SearchResponse},
	settings,
};
use aidoku::{
	HashMap, Result,
	alloc::{string::String, string::ToString, vec::Vec},
	helpers::uri::QueryParameters,
	imports::{
		js::WebView,
		net::{Request, Response},
	},
	prelude::*,
};
use regex::Regex;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;

const GET_VMOBJ_JS: &str = "\
const vmKey = Object.keys(window).find(key => key.startsWith('vm'));\
const vmObj = window[vmKey];\
if (!vmObj || typeof vmObj !== 'object' || vmObj === window) {\
	return '';\
}";

#[allow(dead_code)]
const CANVAS_TO_DATA_URL_TOKEN: &str = "__AIDOKU_CANVAS_TO_DATA_URL_TOKEN__";

// The secure module refuses to paint unless a 2x2 canvas serializes to this exact PNG.
// WebKit's PNG encoder produces a different (but equally valid) representation, causing
// `apply(canvas)` to return normally without drawing anything. This is only returned for the
// module's 2x2, argument-less integrity probe; every real canvas serialization still uses the
// original WebKit implementation captured before the module loads.
#[allow(dead_code)]
const CANVAS_INTEGRITY_DATA_URL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M8AAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

const INSTALLER_REQUEST_TOKEN: &str = "__AIDOKU_INSTALLER_REQUEST_TOKEN__";
const INSTALLER_RESPONSE_TOKEN: &str = "__AIDOKU_INSTALLER_RESPONSE_TOKEN__";

const DESCRAMBLER_BLOB_TOKEN: &str = "__AIDOKU_DESCRAMBLER_BLOB_TOKEN__";
const DESCRAMBLER_CANVAS_TOKEN: &str = "__AIDOKU_DESCRAMBLER_CANVAS_TOKEN__";

#[allow(dead_code)]
const DESCRAMBLER_RESPONSE_TOKEN: &str = "__AIDOKU_DESCRAMBLER_RESPONSE_TOKEN__";
#[allow(dead_code)]
const EMPTY_DESCRAMBLER_RESPONSE_OBJECT: &str =
	"{ data: null, error: null, isDone: false, isAbort: false }";

#[allow(dead_code)]
const FETCH_TIMEOUT_RESPONSE: &str =
	"Fetch timeout after 30s. If problem persist, please restart the application.";

const JS_PATCHER: &str = "<head>\
<script>window['__AIDOKU_CANVAS_TO_DATA_URL_TOKEN__'] = HTMLCanvasElement.prototype.toDataURL;</script>";

const HTML_CAPTURE_PATCH: &str = r#"<head><script>
(() => {
	window.__aidokuBrowsePayload = '';
	window.__aidokuPagePayload = '';
	window.__aidokuCaptureError = '';
	const capture = parsed => {
		try {
			const result = parsed && parsed.result;
			if (
				result && Array.isArray(result.items) &&
				result.items.some(item => item && typeof item.hid === 'string')
			) {
				window.__aidokuBrowsePayload = JSON.stringify(parsed);
			}
			if (result && result.pages && Array.isArray(result.pages.items)) {
				window.__aidokuPagePayload = JSON.stringify(parsed);
			}
		} catch (_) {}
	};
	const captureText = text => {
		try { if (text) capture(JSON.parse(text)); } catch (_) {}
	};
	const originalFetch = window.fetch;
	if (typeof originalFetch === 'function') {
		window.fetch = function () {
			return originalFetch.apply(this, arguments).then(response => {
				try { response.clone().text().then(captureText).catch(() => {}); } catch (_) {}
				return response;
			});
		};
	}
	const originalOpen = XMLHttpRequest.prototype.open;
	const originalSend = XMLHttpRequest.prototype.send;
	XMLHttpRequest.prototype.open = function (method, url) {
		this.__aidokuCaptureUrl = String(url || '');
		return originalOpen.apply(this, arguments);
	};
	XMLHttpRequest.prototype.send = function () {
		this.addEventListener('load', function () {
			try { captureText(this.responseText); } catch (_) {}
		});
		return originalSend.apply(this, arguments);
	};
	const originalParse = JSON.parse;
	JSON.parse = new Proxy(originalParse, {
		apply(target, thisArg, args) {
			const parsed = Reflect.apply(target, thisArg, args);
			capture(parsed);
			return parsed;
		}
	});
	setTimeout(() => {
		window.__aidokuCaptureError = 'Timed out waiting for Comix page data';
	}, 30000);
})();
</script>"#;

const CHAPTER_CAPTURE_JS: &str = r#"(() => {
	window.__aidokuChapterPayload = '';
	window.__aidokuChapterError = '';
	(async () => {
		try {
			const mangaId = __AIDOKU_MANGA_ID__;
			const mainScriptUrl = document.querySelector(
				'script[type="module"][src*="/dist/main-"]'
			)?.src || '';
			if (!mainScriptUrl) throw new Error('Could not find main bundle');
			const mainResponse = await fetch(mainScriptUrl);
			if (!mainResponse.ok) throw new Error('Could not load main bundle');
			const mainJavaScript = await mainResponse.text();
			const environmentFile = mainJavaScript.match(
				/from\s*["']\.\/(env-[^"']+\.js)["']/
			)?.[1];
			if (!environmentFile) throw new Error('Could not find environment bundle');
			const importBundle = new Function('url', 'return import(url)');
			const environment = await importBundle(
				new URL(environmentFile, mainScriptUrl).href
			);
			const mangaApi = Object.values(environment).find(value =>
				value && typeof value === 'object' && typeof value.chapters === 'function'
			);
			if (!mangaApi) throw new Error('Could not find manga API');
			const items = [];
			let page = 1;
			while (page <= 200) {
				const response = await mangaApi.chapters(mangaId, {
					page,
					limit: 100,
					order: { number: 'desc' }
				});
				const pageItems = response && response.items;
				if (!Array.isArray(pageItems) || pageItems.length === 0) break;
				items.push(...pageItems);
				const meta = response.meta || response.pagination || {};
				const lastPage = meta.lastPage || meta.last_page || page;
				if (!(meta.hasNext || page < lastPage)) break;
				page++;
			}
			window.__aidokuChapterPayload = JSON.stringify(items);
		} catch (error) {
			window.__aidokuChapterError = String(error && error.message || error);
		}
	})();
	return '';
})()"#;

const CF_CHALLENGE_ERROR_MESSAGE: &str = "Comix was blocked on this network. Try the other Website Domain in Source Settings, switch to cellular data, or use a VPN, then reload the source.";
const CF_BLOCK_ERROR_MESSAGE: &str = CF_CHALLENGE_ERROR_MESSAGE;

const WAF_CHALLENGE_KEY: &str = "captcha_required";
const WAF_CHALLENGE_ERROR_MESSAGE: &str = CF_CHALLENGE_ERROR_MESSAGE;

#[derive(Deserialize)]
struct AxiosRequest {
	url: String,
	params: Option<HashMap<String, Value>>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct DescrambleResponseObject {
	data: Option<String>,
	error: Option<String>,
}

pub struct ComixWebView {
	web_view: WebView,
	initialized_base: Option<String>,
}

impl ComixWebView {
	pub fn new() -> Self {
		Self {
			web_view: WebView::new(),
			initialized_base: None,
		}
	}

	fn load_webview(&mut self) -> Result<()> {
		let base = settings::base_url();
		self.try_load_webview(&base)?;
		self.initialized_base = Some(base);
		Ok(())
	}

	fn try_load_webview(&mut self, base: &str) -> Result<()> {
		let response = create_request_get(base)?.send()?;

		let html = response.get_string()?;

		if Self::is_cloudflare_block(&html) {
			bail!("{}", CF_BLOCK_ERROR_MESSAGE)
		}

		if Self::is_waf_challenge(&html) {
			bail!("{}", WAF_CHALLENGE_ERROR_MESSAGE)
		}

		self.web_view
			.load_html_blocking(html.replace("<head>", JS_PATCHER).as_str(), Some(base))?;

		if self.find_functions().is_err() {
			self.find_secure_module_src(base)?;
			self.find_functions()?;
		}
		Ok(())
	}

	/// Whether a page is the site's captcha/WAF interstitial rather than the real site.
	fn is_waf_challenge(html: &str) -> bool {
		html.to_lowercase()
			.contains("<title>security check</title>")
	}

	/// Whether Cloudflare rejected the network outright instead of serving a solvable challenge.
	fn is_cloudflare_block(html: &str) -> bool {
		let html = html.to_lowercase();
		html.contains("<title>attention required! | cloudflare</title>")
			|| html.contains("id=\"cf-error-details\"")
	}

	fn get_html(url: &str) -> Result<String> {
		let response = create_request_get(url)?.send()?;
		let html = response.get_string()?;
		if Self::is_cloudflare_block(&html) || Self::is_waf_challenge(&html) {
			bail!("{}", CF_BLOCK_ERROR_MESSAGE)
		}
		Ok(html)
	}

	fn initial_data(html: &str) -> Result<Value> {
		let regex =
			Regex::new(r#"(?s)<script[^>]*id=["']initial-data["'][^>]*>(.*?)</script>"#).unwrap();
		let data = regex
			.captures(html)
			.and_then(|captures| captures.get(1))
			.map(|capture| capture.as_str())
			.ok_or(error!("Could not find initial data in page"))?;
		serde_json::from_str(data).map_err(|e| error!("Invalid initial page data: {e}"))
	}

	fn capture_webview(url: &str, html: String) -> Result<WebView> {
		let patched = if html.contains("<head>") {
			html.replacen("<head>", HTML_CAPTURE_PATCH, 1)
		} else {
			format!("{HTML_CAPTURE_PATCH}{html}")
		};
		let web_view = WebView::new();
		web_view.load_html_blocking(&patched, Some(url))?;
		Ok(web_view)
	}

	fn wait_for_payload(web_view: &WebView, payload_key: &str, error_key: &str) -> Result<String> {
		loop {
			let payload = web_view.eval(&format!("(() => window[{payload_key:?}] || '')()"))?;
			if !payload.is_empty() {
				return Ok(payload);
			}
			let error = web_view.eval(&format!("(() => window[{error_key:?}] || '')()"))?;
			if !error.is_empty() {
				bail!("{error}")
			}
		}
	}

	pub fn fetch_browse(&self, url: &str) -> Result<SearchResponse> {
		let html = Self::get_html(url)?;
		let web_view = Self::capture_webview(url, html)?;
		let payload =
			Self::wait_for_payload(&web_view, "__aidokuBrowsePayload", "__aidokuCaptureError")?;
		serde_json::from_str(&payload).map_err(|e| error!("Invalid browse data: {e}"))
	}

	pub fn fetch_manga(&self, url: &str) -> Result<ComixManga> {
		let html = Self::get_html(url)?;
		let initial_data = Self::initial_data(&html)?;
		initial_data
			.get("queries")
			.and_then(Value::as_object)
			.and_then(|queries| {
				queries
					.iter()
					.find_map(|(key, value)| key.contains("\"detail\"").then(|| value.clone()))
			})
			.ok_or(error!("Could not find manga detail in page"))
			.and_then(|value| {
				serde_json::from_value(value).map_err(|e| error!("Invalid manga detail: {e}"))
			})
	}

	pub fn fetch_chapters(&self, url: &str, manga_id: &str) -> Result<Vec<ComixChapter>> {
		let html = Self::get_html(url)?;
		let web_view = Self::capture_webview(url, html)?;
		let manga_id = serde_json::to_string(manga_id)
			.map_err(|e| error!("Failed to encode manga id: {e}"))?;
		web_view.eval(&CHAPTER_CAPTURE_JS.replace("__AIDOKU_MANGA_ID__", &manga_id))?;
		let payload =
			Self::wait_for_payload(&web_view, "__aidokuChapterPayload", "__aidokuChapterError")?;
		serde_json::from_str(&payload).map_err(|e| error!("Invalid chapter data: {e}"))
	}

	pub fn fetch_pages(&self, url: &str) -> Result<ChapterResponse> {
		let html = Self::get_html(url)?;
		let web_view = Self::capture_webview(url, html)?;
		let payload =
			Self::wait_for_payload(&web_view, "__aidokuPagePayload", "__aidokuCaptureError")?;
		serde_json::from_str(&payload).map_err(|e| error!("Invalid page data: {e}"))
	}

	pub fn has_signer(&self) -> bool {
		self.initialized_base
			.as_deref()
			.is_some_and(|base| base == settings::base_url())
	}

	fn find_secure_module_src(&mut self, base: &str) -> Result<()> {
		let response = create_request_get(base)?.send()?;
		let html = response.get_string()?;
		if Self::is_cloudflare_block(&html) {
			bail!("{}", CF_BLOCK_ERROR_MESSAGE)
		}
		if Self::is_waf_challenge(&html) {
			bail!("{}", WAF_CHALLENGE_ERROR_MESSAGE)
		}
		let main_module_src = response
			.get_html()?
			.select("head > script[type=\"module\"][src*=\"main\"]")
			.and_then(|e| e.first())
			.and_then(|e| e.attr("src"))
			.ok_or(error!("Main module not found"))?;
		if let Some(js_asset_path_index) = main_module_src.rfind("/") {
			let js_asset_path = &main_module_src[0..js_asset_path_index + 1];
			let secure_script_regex = Regex::new("(secure-[A-Za-z0-9-_]+?\\.js)").unwrap();
			let main_module_contents =
				create_request_get(&format!("{base}{main_module_src}"))?.string()?;
			if Self::is_cloudflare_block(&main_module_contents) {
				bail!("{}", CF_BLOCK_ERROR_MESSAGE)
			}
			// this request can be challenged even when the page above wasn't
			if Self::is_waf_challenge(&main_module_contents) {
				bail!("{}", WAF_CHALLENGE_ERROR_MESSAGE)
			}
			if let Some(secure_script_path) = secure_script_regex
				.captures(main_module_contents.as_str())
				.and_then(|captures| captures.get(1).map(|m| m.as_str()))
			{
				// Import the module from its real url first. Importing it from a blob instead
				// makes the descrambler silently draw nothing — `apply()` returns normally and
				// leaves the canvas untouched, so pages render blank with no error. The module
				// evidently checks where it was loaded from, and a `blob:` url fails that check.
				//
				// The blob path below is only a fallback for when the web view can't fetch the
				// module itself. A blocked request leaves `window.vm` empty, which surfaces
				// as "Failed to find installer function". Trading a blank page for a real error
				// is the right way round, so the blob is a last resort rather than the default.
				let secure_url = format!("{base}{js_asset_path}{secure_script_path}");

				self.web_view.eval(&format!(
					"(() => {{
						import('{secure_url}')
							.then((m) => {{ window['vm'] = m; }})
							.catch((e) => {{ window['vm'] = 'failed'; }});
						return '';
					}})()"
				))?;
				while self
					.web_view
					.eval("(() => { return window['vm'] == null ? 'true' : 'false'; })()")?
					== "true"
				{}

				let direct_import_failed = self
					.web_view
					.eval("(() => { return window['vm'] === 'failed' ? 'true' : 'false'; })()")?
					== "true";
				if !direct_import_failed {
					return Ok(());
				}

				// the web view couldn't load it — fetch it over the app's network stack and
				// import it from a blob
				let secure_src = create_request_get(&secure_url)?.string()?;
				if Self::is_cloudflare_block(&secure_src) {
					bail!("{}", CF_BLOCK_ERROR_MESSAGE)
				}
				if Self::is_waf_challenge(&secure_src) {
					bail!("{}", WAF_CHALLENGE_ERROR_MESSAGE)
				}
				let secure_src_literal = serde_json::to_string(&secure_src)
					.map_err(|e| error!("Failed to encode signer module: {e}"))?;

				self.web_view.eval(&format!(
					"(() => {{
						try {{
							const blob = new Blob([{secure_src_literal}], {{ type: 'text/javascript' }});
							const blobUrl = URL.createObjectURL(blob);
							import(blobUrl)
								.then((m) => {{ window['vm'] = m; URL.revokeObjectURL(blobUrl); }})
								.catch((e) => {{ window['vm'] = {{}}; URL.revokeObjectURL(blobUrl); }});
						}} catch (e) {{ window['vm'] = {{}}; }}
						return '';
					}})()"
				))?;
				while self
					.web_view
					.eval("(() => { return window['vm'] === 'failed' ? 'true' : 'false'; })()")?
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

	fn find_functions(&mut self) -> Result<()> {
		let result = self.web_view.eval(&format!(
			"(() => {{
			try {{
				{GET_VMOBJ_JS}
				let fnames = Object.keys(vmObj);
				let inst = '', descBlob = '', descCanvas = '';
				const isPromise = (v) => v && (typeof v === 'object' || typeof v === 'function') && typeof v.then === 'function';
				const canvas = document.createElement('canvas');
				const controller = new AbortController();
                const signal = controller.signal;
				for (let j = 0; j < fnames.length; j++) {{
					let fn = vmObj[fnames[j]];
					if (typeof fn !== 'function') continue;
					let ref = 'window[' + JSON.stringify(vmKey) + '].' + fnames[j];
					if (!inst) {{
						try {{
							let got = false;
							fn({{
								interceptors: {{
									request: {{ use: function() {{ got = true; }} }},
									response: {{ use: function() {{ got = true; }} }}
								}}
							}});
							if (got) {{
								inst = ref;
								fn({{
									interceptors: {{
										request: {{
											use: function (fn) {{ window['{INSTALLER_REQUEST_TOKEN}'] = fn; }},
										}},
										response: {{
											use: function (fn) {{ window['{INSTALLER_RESPONSE_TOKEN}'] = fn; }},
										}},
									}}
								}});
							}}
						}} catch (e) {{}}
					}}
					if (!descCanvas) {{
						try {{
							if (fn.length == 3) {{
								let res = fn('about:blank', canvas, signal);
								if (isPromise(res)) {{
									descCanvas = ref;
									window['{DESCRAMBLER_CANVAS_TOKEN}'] = fn;
								}}
							}}
						}} catch (e) {{}}
					}}
					if (!descBlob) {{
						try {{
							if (fn.length == 2) {{
								let res = fn('about:blank', signal);
								if (isPromise(res)) {{
									descBlob = ref;
									window['{DESCRAMBLER_BLOB_TOKEN}'] = fn;
								}}
							}}
						}} catch (e) {{}}
					}}
				}}
				return inst + '||' + descCanvas + '||' + descBlob;
			}} catch (e) {{}}
			return '';
		}})()",
		))?;
		let expr: Vec<&str> = result.split("||").collect();
		if expr.is_empty() || expr[0].is_empty() {
			bail!("Failed to find installer function");
		}
		// Comix's current secure module keeps the request installer but no longer exports the
		// legacy canvas/blob descrambler. Current v3 images are restored natively from their
		// x-scramble-* response headers in process_page_image.
		Ok(())
	}

	pub fn build_request(&mut self, url: &str) -> Result<Request> {
		if !self.has_signer() {
			self.load_webview()?
		}

		let result = self.web_view.eval(&format!(
			"(() => {{
			const url = new URL('{url}');
			const result = {{}};

			for (const [key, rawValue] of url.searchParams) {{
				const value = /^\\d+$/.test(rawValue)
					? Number(rawValue)
					: rawValue;

				const parts = key.replace(/\\]/g, '').split('[');

				let current = result;

				for (let i = 0; i < parts.length; i++) {{
					const part = parts[i];
					const last = i === parts.length - 1;

					if (last) {{
						if (part === '') {{
							current.push(value);
						}} else if (current[part] === undefined) {{
							current[part] = value;
						}} else if (Array.isArray(current[part])) {{
							current[part].push(value);
						}} else {{
							current[part] = [current[part], value];
						}}
					}} else {{
						const nextPart = parts[i + 1];

						current[part] ??= nextPart === '' ? [] : {{}};
						current = current[part];
					}}
				}}
			}}

			const request = window['{INSTALLER_REQUEST_TOKEN}']({{
				url: `${{url.origin}}${{url.pathname}}`,
				method: 'GET',
				params: result,
			}});

			return JSON.stringify(request);
		}})()"
		))?;

		let axios_request: AxiosRequest = serde_json::from_str(result.as_str())?;

		fn build_query(params_map: &HashMap<String, Value>) -> QueryParameters {
			let mut params = QueryParameters::new();

			for (key, value) in params_map {
				push_value(&mut params, key, value);
			}

			params
		}

		fn push_value(params: &mut QueryParameters, key: &str, value: &Value) {
			match value {
				Value::Null => {
					params.push_key(key);
				}

				Value::Bool(_) | Value::Number(_) | Value::String(_) => {
					let value_str = value.to_string();

					// Remove JSON string quotes
					let value_str = match value {
						Value::String(s) => s.as_str(),
						_ => value_str.as_str(),
					};

					params.push(key, Some(value_str));
				}

				Value::Array(arr) => {
					let array_key = format!("{key}[]");

					for item in arr {
						match item {
							Value::String(s) => {
								params.push(&array_key, Some(s));
							}
							_ => {
								let value_str = item.to_string();
								params.push(&array_key, Some(&value_str));
							}
						}
					}
				}

				Value::Object(obj) => {
					for (child_key, child_value) in obj {
						let nested_key = format!("{key}[{child_key}]");
						push_value(params, &nested_key, child_value);
					}
				}
			}
		}

		if let Some(params) = axios_request.params {
			let query = build_query(&params);
			create_request_get(&format!("{}?{query}", axios_request.url))
		} else {
			create_request_get(&axios_request.url)
		}
	}

	pub fn decode_json_owned<T>(&mut self, response: &Response) -> Result<T>
	where
		T: DeserializeOwned,
	{
		if !self.has_signer() {
			self.load_webview()?;
		}

		let status_code = response.status_code();

		if status_code == 403
			&& response
				.get_header("cf-mitigated")
				.is_some_and(|value| value == "challenge")
		{
			bail!("{CF_CHALLENGE_ERROR_MESSAGE}")
		} else if status_code >= 400 {
			if response.status_code() == 403
				&& serde_json::from_slice::<ErrorResponse>(&response.get_data()?)
					.is_ok_and(|e| e.error == WAF_CHALLENGE_KEY)
			{
				bail!("{}", WAF_CHALLENGE_ERROR_MESSAGE)
			} else {
				bail!("Response Error: {}", response.status_code())
			}
		} else if response
			.get_header("x-enc")
			.is_some_and(|value| value == "1")
		{
			let encoded_response = response
				.get_string()?
				.replace("\\", "\\\\")
				.replace("'", "\\'");

			let result = self.web_view.eval(&format!(
				"(() => {{
					try {{
						let decoded = window['{INSTALLER_RESPONSE_TOKEN}']({{
							data: JSON.parse('{encoded_response}'),
							status: 200,
							headers: {{
								'x-enc': '1',
							}},
						}});
						return JSON.stringify({{ result: decoded && decoded.data }});
					}} catch(e) {{
						return 'error: ' + e;
					}}
				}})()",
			))?;

			if result.starts_with("error:") {
				bail!("{result}");
			} else if result.is_empty() {
				bail!("Failed to fetch result")
			}

			serde_json::from_str(&result).map_err(|e| error!("Invalid json: {}", e))
		} else {
			let json_str = response.get_string()?;
			serde_json::from_str(&json_str).map_err(|e| error!("Invalid json: {}", e))
		}
	}

	#[allow(dead_code)]
	pub fn descramble_image(&mut self, width: f32, height: f32, url: &str) -> Result<String> {
		if !self.has_signer() {
			self.load_webview()?
		}

		self.web_view.eval(&format!(
			"(() => {{
				window['{DESCRAMBLER_RESPONSE_TOKEN}'] = {EMPTY_DESCRAMBLER_RESPONSE_OBJECT};

				// Comix gates its painter behind an exact canvas-encoding fingerprint. Limit
				// the compatibility value to that 2x2 probe so image processing and output
				// continue to use WebKit's real toDataURL implementation.
				const applyDescrambler = (data, canvas) => {{
					const currentToDataURL = HTMLCanvasElement.prototype.toDataURL;
					try {{
						HTMLCanvasElement.prototype.toDataURL = function (...args) {{
							if (this.width === 2 && this.height === 2 && args.length === 0) {{
								return '{CANVAS_INTEGRITY_DATA_URL}';
							}}
							return currentToDataURL.apply(this, args);
						}};
						data.apply(canvas);
					}} finally {{
						HTMLCanvasElement.prototype.toDataURL = currentToDataURL;
					}}
				}};

				const controller = new AbortController();
                const signal = controller.signal;

				const canvas = document.createElement('canvas');
				canvas.width = {width};
				canvas.height = {height};

				const timeout = setTimeout(() => {{
					controller.abort();
					window['{DESCRAMBLER_RESPONSE_TOKEN}'].isAbort = true;
				}}, 30000);

				if (window['{DESCRAMBLER_BLOB_TOKEN}'] != null) {{
					window['{DESCRAMBLER_BLOB_TOKEN}']('{url}', signal)
						.then((data) => {{
							if (typeof data === 'object' && data.mode && typeof data.mode === 'string') {{
								if (data.mode === 'blob') {{
									return new Promise((resolve, reject) => {{
										const url = URL.createObjectURL(data.blob);
										const image = new Image();
										image.src = url;
										image.onload = () => resolve(image);
										image.onerror = reject;
									}})
								}} else if (data.mode === 'canvas') {{
									applyDescrambler(data, canvas)
									const output = window['{CANVAS_TO_DATA_URL_TOKEN}'].call(canvas);
									window['{DESCRAMBLER_RESPONSE_TOKEN}'].data = output;
									window['{DESCRAMBLER_RESPONSE_TOKEN}'].isDone = true;
									clearTimeout(timeout);
								}} else {{
									throw new Exception('Unknown data mode. Maybe comix tried something new again?');
								}}
								return null;
							}} else if (typeof data === 'object' && data.apply && typeof data.apply === 'function') {{
								applyDescrambler(data, canvas)
								const output = window['{CANVAS_TO_DATA_URL_TOKEN}'].call(canvas);
								window['{DESCRAMBLER_RESPONSE_TOKEN}'].data = output;
								window['{DESCRAMBLER_RESPONSE_TOKEN}'].isDone = true;
								clearTimeout(timeout);
								return null;
							}} else if (typeof data === 'object' && data.blob) {{
								return new Promise((resolve, reject) => {{
									const url = URL.createObjectURL(data.blob);
									const image = new Image();
									image.src = url;
									image.onload = () => resolve(image);
									image.onerror = reject;
								}})
							}} else {{
								return new Promise((resolve, reject) => {{
									const url = URL.createObjectURL(data);
									const image = new Image();
									image.src = url;
									image.onload = () => resolve(image);
									image.onerror = reject;
								}})
							}}
						}})
						.then((obj) => {{
							if (typeof obj === 'object' && obj) {{
								URL.revokeObjectURL(obj.src);
								const ctx = canvas.getContext('2d');
								ctx.drawImage(obj, 0, 0);
								const data = window['{CANVAS_TO_DATA_URL_TOKEN}'].call(canvas);
								window['{DESCRAMBLER_RESPONSE_TOKEN}'].data = data;
								window['{DESCRAMBLER_RESPONSE_TOKEN}'].isDone = true;
								clearTimeout(timeout);
							}}
						}})
						.catch((error) => {{
							if (window['{DESCRAMBLER_CANVAS_TOKEN}'] != null) {{
								window['{DESCRAMBLER_CANVAS_TOKEN}']('{url}', canvas, signal)
									.then(() => {{
										const data = window['{CANVAS_TO_DATA_URL_TOKEN}'].call(canvas);
										window['{DESCRAMBLER_RESPONSE_TOKEN}'].data = data;
									}})
									.catch((error) => {{
										window['{DESCRAMBLER_RESPONSE_TOKEN}'].error = error.message;
									}})
									.finally(() => {{
										window['{DESCRAMBLER_RESPONSE_TOKEN}'].isDone = true;
										clearTimeout(timeout);
									}});
							}} else {{
								window['{DESCRAMBLER_RESPONSE_TOKEN}'].error = error.message;
								window['{DESCRAMBLER_RESPONSE_TOKEN}'].isDone = true;
								clearTimeout(timeout);
							}}
						}});
				}} else if (window['{DESCRAMBLER_CANVAS_TOKEN}'] != null) {{
					window['{DESCRAMBLER_CANVAS_TOKEN}']('{url}', canvas, signal)
						.then(() => {{
							const data = window['{CANVAS_TO_DATA_URL_TOKEN}'].call(canvas);
							window['{DESCRAMBLER_RESPONSE_TOKEN}'].data = data;
						}})
						.catch((error) => {{
							window['{DESCRAMBLER_RESPONSE_TOKEN}'].error = error.message;
						}})
						.finally(() => {{
							window['{DESCRAMBLER_RESPONSE_TOKEN}'].isDone = true;
							clearTimeout(timeout);
						}});
				}} else {{
					window['{DESCRAMBLER_RESPONSE_TOKEN}'].error = 'No suitable descrambler found.';
					window['{DESCRAMBLER_RESPONSE_TOKEN}'].isDone = true;
					clearTimeout(timeout);
				}}

				return '';
			}})()"
		))?;

		while self.web_view.eval(&format!(
			"(() => {{ return window['{DESCRAMBLER_RESPONSE_TOKEN}'].isDone ? 'true' : 'false'; }})()"
		))? == "false"
		{
			if self.web_view.eval(&format!(
				"(() => {{ return window['{DESCRAMBLER_RESPONSE_TOKEN}'].isAbort ? 'true' : 'false'; }})()"
			))? == "true"
			{
				self.load_webview()?;
				bail!("{FETCH_TIMEOUT_RESPONSE}");
			}
		}

		let result = self.web_view.eval(&format!(
			"(() => {{ return JSON.stringify(window['{DESCRAMBLER_RESPONSE_TOKEN}']); }})()"
		))?;

		let json = serde_json::from_str::<DescrambleResponseObject>(&result)?;

		if let Some(error) = json.error {
			bail!("{error}");
		}

		json.data.ok_or(error!("Fetch data is null"))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use aidoku_test::aidoku_test;

	#[aidoku_test]
	fn recognizes_cloudflare_hard_block() {
		assert!(ComixWebView::is_cloudflare_block(
			"<title>Attention Required! | Cloudflare</title><div id=\"cf-error-details\">"
		));
		assert!(!ComixWebView::is_cloudflare_block(
			"<title>Comix - Read Comics online for free</title>"
		));
		assert!(!ComixWebView::is_cloudflare_block(
			"<title>Comix - Read Comics online for free</title><p>Sorry, you have been blocked</p>"
		));
	}
}
