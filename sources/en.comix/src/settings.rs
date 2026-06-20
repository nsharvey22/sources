use aidoku::{
	alloc::{string::String, vec::Vec},
	imports::defaults::{DefaultValue, defaults_get, defaults_set},
};

const HIDE_NSFW_KEY: &str = "hideNSFW";
const THUMBNAIL_QUALITY_KEY: &str = "thumbnailQuality";
const DEDUPED_CHAPTER_KEY: &str = "dedupedChapter";

const HIDDEN_TYPES_KEY: &str = "hiddenTypes";
const HIDDEN_GENRES_KEY: &str = "hiddenGenres";
const HIDDEN_THEMES_KEY: &str = "hiddenThemes";

pub fn hide_nsfw() -> bool {
	defaults_get::<bool>(HIDE_NSFW_KEY).unwrap_or(true)
}

pub fn content_ratings() -> &'static [&'static str] {
	if hide_nsfw() {
		&["safe", "suggestive"]
	} else {
		&["safe", "suggestive", "erotica", "pornographic"]
	}
}

pub fn content_rating_qs() -> String {
	content_ratings()
		.iter()
		.map(|r| {
			let mut s = String::from("content_rating[]=");
			s.push_str(r);
			s
		})
		.collect::<Vec<_>>()
		.join("&")
}

pub fn image_quality() -> String {
	defaults_get::<String>(THUMBNAIL_QUALITY_KEY).unwrap_or_default()
}

pub fn dedupchapter() -> bool {
	defaults_get::<bool>(DEDUPED_CHAPTER_KEY).unwrap_or(false)
}

pub fn hidden_types() -> Vec<String> {
	defaults_get::<Vec<String>>(HIDDEN_TYPES_KEY).unwrap_or_default()
}

pub fn hidden_terms() -> Vec<i32> {
	hidden_genres().into_iter().chain(hidden_themes()).collect()
}

fn hidden_genres() -> Vec<i32> {
	defaults_get::<Vec<String>>(HIDDEN_GENRES_KEY)
		.unwrap_or_default()
		.into_iter()
		.filter_map(|s| s.parse().ok())
		.collect()
}

fn hidden_themes() -> Vec<i32> {
	defaults_get::<Vec<String>>(HIDDEN_THEMES_KEY)
		.unwrap_or_default()
		.into_iter()
		.filter_map(|s| s.parse().ok())
		.collect()
}

pub fn reset_filters() {
	defaults_set(HIDDEN_TYPES_KEY, DefaultValue::Null);
	defaults_set(HIDDEN_GENRES_KEY, DefaultValue::Null);
	defaults_set(HIDDEN_THEMES_KEY, DefaultValue::Null);
}
