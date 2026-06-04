#[macro_use]
extern crate rust_i18n;

i18n!("locales", fallback = "zh");

pub mod extract;
pub mod i18n;
pub mod images;
pub mod split;
