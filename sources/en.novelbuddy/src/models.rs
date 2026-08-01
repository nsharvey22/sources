use aidoku::alloc::{String, Vec};
use serde::{Deserialize, Deserializer};

/// The API has served `is_adult` both as an integer (0/1, pre-migration) and as a
/// JSON boolean (post-migration to novelbuddy.me). Accept either so the next
/// serializer change doesn't break every title-detail fetch again.
fn bool_or_int<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
	struct Visitor;
	impl serde::de::Visitor<'_> for Visitor {
		type Value = bool;
		fn expecting(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
			f.write_str("a boolean or an integer")
		}
		fn visit_bool<E>(self, v: bool) -> Result<bool, E> {
			Ok(v)
		}
		fn visit_i64<E>(self, v: i64) -> Result<bool, E> {
			Ok(v != 0)
		}
		fn visit_u64<E>(self, v: u64) -> Result<bool, E> {
			Ok(v != 0)
		}
	}
	deserializer.deserialize_any(Visitor)
}

#[derive(Deserialize)]
pub struct ApiResponse<T> {
	#[serde(default)]
	pub success: bool,
	pub message: Option<String>,
	pub data: Option<T>,
}

#[derive(Deserialize)]
pub struct ListData {
	pub items: Vec<TitleListItem>,
	pub pagination: Pagination,
}

#[derive(Deserialize)]
pub struct Pagination {
	#[serde(default)]
	pub has_next: bool,
}

#[derive(Deserialize)]
pub struct TrendingData {
	pub items: Vec<TitleListItem>,
}

#[derive(Deserialize)]
pub struct TitleListItem {
	pub id: String,
	pub name: String,
	#[serde(default)]
	pub slug: Option<String>,
	#[serde(default)]
	pub cover: Option<String>,
}

#[derive(Deserialize)]
pub struct TitleDetailData {
	pub title: TitleDetail,
}

#[derive(Deserialize)]
pub struct TitleDetail {
	pub id: String,
	pub name: String,
	#[serde(default)]
	pub slug: Option<String>,
	#[serde(default)]
	pub summary: Option<String>,
	#[serde(default)]
	pub cover: Option<String>,
	#[serde(default)]
	pub status: Option<String>,
	#[serde(default)]
	pub genres: Vec<NamedSlug>,
	#[serde(default)]
	pub authors: Vec<NamedSlug>,
	#[serde(default)]
	pub artists: Vec<NamedSlug>,
	#[serde(default)]
	pub tags: Vec<NamedSlug>,
	#[serde(default, deserialize_with = "bool_or_int")]
	pub is_adult: bool,
}

#[derive(Deserialize)]
pub struct NamedSlug {
	pub name: String,
}

#[derive(Deserialize)]
pub struct ChapterListData {
	pub chapters: Vec<ChapterListItem>,
}

#[derive(Deserialize)]
pub struct ChapterListItem {
	pub id: String,
	pub name: String,
	#[serde(default)]
	pub url: Option<String>,
	#[serde(default)]
	pub updated_at: Option<String>,
	/// Added by the novelbuddy.me API; older payloads lack it, so name parsing
	/// stays as the fallback.
	#[serde(default)]
	pub number: Option<f32>,
}

#[derive(Deserialize)]
pub struct ChapterDetailData {
	pub chapter: ChapterDetail,
}

#[derive(Deserialize)]
pub struct ChapterDetail {
	#[serde(default)]
	pub content: Option<String>,
}

#[derive(Deserialize)]
pub struct BySlugData {
	pub new_url: String,
}
