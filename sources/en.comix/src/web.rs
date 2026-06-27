use crate::{API_URL, BASE_URL};
use aidoku::{
	Result,
	alloc::{String, Vec, string::ToString},
	imports::{js::WebView, net::Request},
	prelude::*,
};
use regex::Regex;

// reference: https://github.com/nobottomline/extensions-source/blob/c8fe930f315f3baee23587559edfceab5e969202/src/en/comix/src/eu/kanade/tachiyomi/extension/en/comix/Signer.kt

const GET_VMOBJ_JS: &str = "\
const vmKey = Object.keys(window).find(key => key.startsWith('vm'));\
const vmObj = window[vmKey];\
if (!vmObj || typeof vmObj !== 'object' || vmObj === window) {\
	return '';\
}";

const JS_PATCHER: &str = "<head><script>window['originalGetImageData'] = HTMLCanvasElement.prototype.toDataURL;</script>";

pub struct ComixWebView {
	pub web_view: WebView,
	pub descrambler_fn: Option<String>,
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
	let js_asset_path_index = main_module_src
		.rfind('/')
		.ok_or(error!("Invalid module path"))?;
	let js_asset_path = &main_module_src[0..js_asset_path_index + 1];
	let secure_script_regex = Regex::new("(secure-[A-Za-z0-9-_]+?\\.js)").unwrap();
	let main_module_contents = Request::get(format!("{BASE_URL}{main_module_src}"))?.string()?;
	let secure_script_path = secure_script_regex
		.captures(main_module_contents.as_str())
		.and_then(|c| c.get(1).map(|m| m.as_str()))
		.ok_or(error!("Secure module not found"))?;

	// Fetch the secure module over the network stack (which carries a valid cf_clearance)
	// rather than letting the WebView fetch it via import() of the remote URL. On some
	// devices Cloudflare challenges the WebView's own asset fetch, so the remote import
	// silently fails and the signer functions never load. We import the fetched source via
	// a blob URL instead, which executes it as a module without another network request.
	let secure_url = format!("{BASE_URL}{js_asset_path}{secure_script_path}");
	let secure_src = Request::get(&secure_url)?.string()?;
	let secure_src_literal = serde_json::to_string(&secure_src)
		.map_err(|e| error!("Failed to encode secure module: {e}"))?;
	web_view.web_view.eval(&format!(
		"(() => {{\
			try {{\
				const blob = new Blob([{secure_src_literal}], {{ type: 'text/javascript' }});\
				const blobUrl = URL.createObjectURL(blob);\
				import(blobUrl)\
					.then((m) => {{ window['vm'] = m; URL.revokeObjectURL(blobUrl); }})\
					.catch((e) => {{ window['vm'] = {{}}; URL.revokeObjectURL(blobUrl); }});\
			}} catch (e) {{ window['vm'] = {{}}; }}\
			return '';\
		}})()"
	))?;
	while web_view
		.web_view
		.eval("(() => { return window['vm'] == null ? 'true' : 'false'; })()")?
		== "true"
	{}
	Ok(())
}

fn find_functions(web_view: &mut ComixWebView) -> Result<()> {
	let result = web_view.web_view.eval(&format!(
		"(() => {{\
			try {{\
				{GET_VMOBJ_JS}\
				let fnames = Object.keys(vmObj);\
				let installerFn = null, descFn = null;\
				let log = [];\
				const isPromise = (v) => v && (typeof v === 'object' || typeof v === 'function') && typeof v.then === 'function';\
				const testCanvas = document.createElement('canvas');\
				for (let j = 0; j < fnames.length; j++) {{\
					let fn = vmObj[fnames[j]];\
					if (typeof fn !== 'function') continue;\
					if (!installerFn) {{\
						try {{\
							let reqGot = false, resGot = false;\
							fn({{\
								interceptors: {{\
									request: {{ use: function() {{ reqGot = true; }} }},\
									response: {{ use: function() {{ resGot = true; }} }}\
								}},\
								defaults: {{\
									headers: {{ common: {{}} }},\
									transformRequest: [],\
									transformResponse: []\
								}}\
							}});\
							if (reqGot) {{\
								log.push(fnames[j] + ':' + (resGot ? 'both' : 'req'));\
								installerFn = fn;\
							}}\
						}} catch (e) {{}}\
					}}\
					if (!descFn) {{\
						try {{\
							if (fn.constructor && fn.constructor.name === 'AsyncFunction') {{\
								descFn = fn;\
							}} else if (fn.length >= 2) {{\
								let res = fn('about:blank', testCanvas);\
								if (isPromise(res)) descFn = fn;\
							}}\
						}} catch (e) {{}}\
					}}\
				}}\
				if (!installerFn) return 'error: no installer';\
				if (!descFn) return 'error: no descrambler';\
				let capturedReq = null, capturedRes = null;\
				installerFn({{\
					interceptors: {{\
						request: {{ use: function(fn) {{ capturedReq = fn; }} }},\
						response: {{ use: function(fn) {{ capturedRes = fn; }} }}\
					}},\
					defaults: {{\
						headers: {{ common: {{}} }},\
						transformRequest: [],\
						transformResponse: []\
					}}\
				}});\
				if (!capturedReq) return 'error: no request interceptor captured';\
				window['__comixReqFn'] = capturedReq;\
				window['__comixResFn'] = capturedRes;\
				window['__comixDesc'] = descFn;\
				return JSON.stringify({{ log: log.join(',') }});\
			}} catch(e) {{ return 'error:' + e; }}\
			return '';\
		}})()"
	))?;
	println!("[comix] find_functions: {result}");
	if result.starts_with("error:") {
		bail!("{result}");
	}
	let parsed = serde_json::from_str::<serde_json::Value>(&result)
		.map_err(|e| error!("find_functions parse error: {e} raw={result}"))?;
	println!("[comix] find_functions candidates: {}", parsed["log"].as_str().unwrap_or(""));
	web_view.descrambler_fn = Some("window['__comixDesc']".into());
	Ok(())
}

/// Like `get_token_with_params` but with empty params (path-only token).
/// produces a full-length token (needed when params are part of the signed input).
pub fn get_token_with_params(web_view: &ComixWebView, path: &str, params_json: &str) -> Result<String> {
	let token = web_view.web_view.eval(&format!(
		"(() => {{\
			try {{\
				const reqFn = window['__comixReqFn'];\
				if (!reqFn) return 'error: __comixReqFn not set';\
				const config = {{ url: '{path}', method: 'GET', params: {params_json} }};\
				const result = reqFn(config);\
				return result.params['_'] || '';\
			}} catch(e) {{\
				return 'error: ' + e;\
			}}\
		}})()"
	))?;
	if token.is_empty() {
		bail!("Failed to fetch token")
	}
	if token.starts_with("error:") {
		bail!("{token}")
	}
	Ok(token)
}

/// Build a JSON params object from a URL-encoded query string.
/// Handles percent-decoded bracket keys (e.g. `order%5Bscore%5D` → `order[score]`)
/// and duplicate/array keys (e.g. `types[]=manhwa&types[]=manhua` → `"types":["manhwa","manhua"]`).
/// Keys ending with `[]` are always emitted as JSON arrays (even with one value), matching
/// the real axios params object that the WASM signer receives.
fn query_to_params_json(query: &str) -> String {
	if query.is_empty() {
		return "{}".into();
	}
	// (key, values, is_array) — is_array=true when the URL key had trailing []
	let mut map: Vec<(String, Vec<String>, bool)> = Vec::new();

	for pair in query.split('&') {
		let (raw_key, raw_val) = pair.split_once('=').unwrap_or((pair, ""));
		let decoded_key = percent_decode(raw_key);
		// axios serializes { types: [...] } as types[]=val; strip trailing [] to recover the real key
		let (key, is_array) = if decoded_key.ends_with("[]") {
			(decoded_key[..decoded_key.len() - 2].to_string(), true)
		} else {
			(decoded_key, false)
		};
		let val = percent_decode(raw_val);
		if let Some(entry) = map.iter_mut().find(|(k, _, _)| k == &key) {
			entry.1.push(val);
			entry.2 = entry.2 || is_array;
		} else {
			map.push((key, { let mut v = Vec::new(); v.push(val); v }, is_array));
		}
	}

	let mut parts: Vec<String> = Vec::new();
	for (key, values, is_array) in map {
		let key_json = json_escape(&key);
		if !is_array && values.len() == 1 {
			parts.push(format!("\"{}\":{}", key_json, json_value(&values[0])));
		} else {
			let arr: Vec<String> = values.iter().map(|v| json_value(v)).collect();
			parts.push(format!("\"{}\":[{}]", key_json, arr.join(",")));
		}
	}
	format!("{{{}}}", parts.join(","))
}

fn hex_nibble(b: u8) -> u8 {
	match b {
		b'0'..=b'9' => b - b'0',
		b'a'..=b'f' => b - b'a' + 10,
		b'A'..=b'F' => b - b'A' + 10,
		_ => 16,
	}
}

fn percent_decode(s: &str) -> String {
	let mut result = String::with_capacity(s.len());
	let bytes = s.as_bytes();
	let mut i = 0;
	let mut raw: Vec<u8> = Vec::new();

	while i < bytes.len() {
		if bytes[i] == b'%' && i + 2 < bytes.len() {
			let hi = hex_nibble(bytes[i + 1]);
			let lo = hex_nibble(bytes[i + 2]);
			if hi < 16 && lo < 16 {
				raw.push((hi << 4) | lo);
				i += 3;
				match core::str::from_utf8(&raw) {
					Ok(s) => { result.push_str(s); raw.clear(); }
					Err(e) if e.error_len().is_none() => {} // incomplete multi-byte seq, keep accumulating
					Err(_) => { for b in raw.drain(..) { result.push(b as char); } }
				}
				continue;
			}
		}
		if !raw.is_empty() {
			match core::str::from_utf8(&raw) {
				Ok(s) => result.push_str(s),
				Err(_) => { for b in &raw { result.push(*b as char); } }
			}
			raw.clear();
		}
		result.push(if bytes[i] == b'+' { ' ' } else { bytes[i] as char });
		i += 1;
	}
	if !raw.is_empty() {
		if let Ok(s) = core::str::from_utf8(&raw) { result.push_str(s); }
	}
	result
}

fn json_escape(s: &str) -> String {
	s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Render a query-string value as a JSON literal.
/// Integer-like values (no decimal point, optional leading `-`) are emitted
/// as JSON numbers so they match the numeric types in the real axios params.
fn json_value(s: &str) -> String {
	if s.parse::<i64>().is_ok() {
		s.to_string()
	} else {
		format!("\"{}\"", json_escape(s))
	}
}

/// Make an authenticated GET request to a comix.to API URL.
/// Automatically generates the `_` token from the URL's path and query params,
/// appends it to the request, and returns the raw response string.
pub fn fetch_api(web_view: &ComixWebView, url: &str) -> Result<String> {
	let without_base = url.strip_prefix(API_URL).unwrap_or(url);
	let (path, query) = without_base.split_once('?').unwrap_or((without_base, ""));

	let params_json = query_to_params_json(query);
	println!("[comix] fetch_api path={path} params={params_json}");
	let token = get_token_with_params(web_view, path, &params_json)?;
	println!("[comix] fetch_api token: len={} val='{token}'", token.len());

	let full_url = if query.is_empty() {
		format!("{url}?_={token}")
	} else {
		format!("{url}&_={token}")
	};

	Request::get(&full_url)?.string()
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
		"(() => {{\
		const canvas = document.createElement('canvas');\
		canvas.width = {width};\
		canvas.height = {height};\
		window['TEMP_CANVAS'] = canvas;\
		window['TEMP_STATE'] = {{ isDone: false, error: null }};\
		const controller = new AbortController();\
		const signal = controller.signal;\
		{GET_VMOBJ_JS}\
		{descrambler_fn}('{url}', signal)\
			.then((result) => {{\
				if (result && typeof result === 'object' && typeof result.apply === 'function') {{\
					/* current API: descrambler resolves to an object that paints the \
					   descrambled image onto a canvas element via apply(canvas) */\
					result.apply(canvas);\
					window['TEMP_STATE'].isDone = true;\
				}} else {{\
					/* legacy API: resolves to a Blob/image source */\
					const objUrl = URL.createObjectURL(result);\
					const image = new Image();\
					image.onload = () => {{\
						URL.revokeObjectURL(objUrl);\
						canvas.getContext('2d').drawImage(image, 0, 0);\
						window['TEMP_STATE'].isDone = true;\
					}};\
					image.onerror = () => {{ window['TEMP_STATE'].isDone = true; window['TEMP_STATE'].error = 'image load failed'; }};\
					image.src = objUrl;\
				}}\
			}})\
			.catch((e) => {{ window['TEMP_STATE'].isDone = true; window['TEMP_STATE'].error = '' + e }});\
		return '';\
	}})()"
	))?;

	while web_view
		.web_view
		.eval("(() => { return window['TEMP_STATE'].isDone ? 'true' : 'false'; })()")?
		== "false"
	{}

	let result = web_view.web_view.eval(
		"(() => {\
		if (window['TEMP_STATE'].error) return 'ERR:' + window['TEMP_STATE'].error;\
		const data = window['originalGetImageData'].call(window['TEMP_CANVAS']);\
		if (!data) return 'ERR: empty canvas data';\
		return data;\
	})()",
	)?;

	if let Some(err) = result.strip_prefix("ERR:") {
		bail!("descramble failed:{err}")
	}
	if result.is_empty() {
		bail!("Failed to descramble image (empty result)")
	}
	Ok(result)
}

pub fn decode_response(web_view: &ComixWebView, url: &str, encoded_res: &str) -> Result<String> {
	let json = serde_json::from_str::<serde_json::Value>(encoded_res)
		.map_err(|_| error!("Invalid api response"))?;
	let is_encoded = match json {
		serde_json::Value::Object(ref map) => map.contains_key("e"),
		_ => false,
	};
	if !is_encoded {
		return Ok(encoded_res.into());
	}
	let encoded_res_escaped = encoded_res.replace('\'', "\\'");
	let result = web_view.web_view.eval(&format!(
		"(() => {{\
			try {{\
				const resFn = window['__comixResFn'];\
				if (!resFn) {{ return 'error: __comixResFn not set'; }}\
				let raw = JSON.parse('{encoded_res_escaped}');\
				let fakeResp = {{\
					data: raw,\
					status: 200,\
					statusText: '',\
					headers: {{ 'x-enc': '1' }},\
					config: {{ url: '{url}', method: 'get', baseURL: '/api/v1' }},\
					request: {{}},\
				}};\
				let decoded = resFn(fakeResp);\
				return JSON.stringify({{ result: decoded && decoded.data }});\
			}} catch(e) {{\
				return 'error: ' + e;\
			}}\
		}})()"
	))?;
	if result.starts_with("error:") {
		bail!("{result}");
	} else if result.is_empty() {
		bail!("Failed to decode response")
	}
	Ok(result)
}
