//! Report writers. Every file they create goes through `sink::OutputDir`.

pub mod csv;
pub mod html;
pub mod json;
pub mod payload;
pub mod sink;
pub mod terminal;
