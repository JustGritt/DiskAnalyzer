//! Report writers. Every file they create goes through `sink::OutputDir`.

pub mod csv;
pub mod html;
pub mod sink;
pub mod terminal;
