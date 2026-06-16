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
	web_view.web_view.eval(&format!(
		"(() => {{\
			import('{BASE_URL}{js_asset_path}{secure_script_path}')\
				.then((m) => window['vm'] = m)\
				.catch((e) => window['vm'] = {{}});\
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

fn percent_decode(s: &str) -> String {
	s.replace("%5B", "[")
		.replace("%5b", "[")
		.replace("%5D", "]")
		.replace("%5d", "]")
		.replace("%2C", ",")
		.replace("%2c", ",")
		.replace("%20", " ")
		.replace("%2B", "+")
		.replace("%2b", "+")
		.replace("%3A", ":")
		.replace("%3a", ":")
		.replace('+', " ")
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

#[allow(dead_code)]
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
			.then((data) => {{\
				const url = URL.createObjectURL(data);\
				const image = new Image();\
				image.src = url;\
				image.onload = () => {{\
					URL.revokeObjectURL(url);\
					const ctx = canvas.getContext('2d');\
					ctx.drawImage(image, 0, 0);\
					window['TEMP_STATE'].isDone = true;\
				}}\
			}})\
			.catch((e) => {{ window['TEMP_STATE'].isDone = true; window['TEMP_STATE'].error = e }});\
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
		if (window['TEMP_STATE'].error) return '';\
		const data = window['originalGetImageData'].call(window['TEMP_CANVAS']);\
		return data;\
	})()",
	)?;

	if result.is_empty() {
		bail!("Failed to descramble image")
	} else {
		Ok(result)
	}
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
