//! A single self-contained HTML report.
//!
//! Everything is inlined into one file: no CDN, no network, no companion
//! assets. The treemap is the point of this report: a table tells you the
//! ten biggest folders; a treemap shows the shape of the whole drive at
//! once. The page itself is `template.html`; it reads the document built by
//! `payload`.

use std::io::{self, Write};

use super::payload::build_payload;
use super::sink::OutputDir;
use crate::insights::Report;
use crate::scanner::ScanResult;
use crate::util::Json;

const TEMPLATE: &str = include_str!("template.html");

/// The complete document for an already built payload.
pub fn render_payload(payload: &Json) -> String {
    // `</script>` inside the JSON would end the block early; escaping the
    // slash keeps the document valid without changing what JSON.parse sees.
    let blob = payload.to_json_string().replace("</", "<\\/");
    TEMPLATE.replace("__PAYLOAD__", &blob)
}

/// The complete document as a string.
pub fn render(report: &Report, result: &ScanResult) -> String {
    render_payload(&build_payload(report, result))
}

pub fn write_html_payload(payload: &Json, out: &OutputDir) -> io::Result<()> {
    let mut file = out.create_file("report.html")?;
    file.write_all(render_payload(payload).as_bytes())?;
    file.flush()
}

pub fn write_html(report: &Report, result: &ScanResult, out: &OutputDir) -> io::Result<()> {
    write_html_payload(&build_payload(report, result), out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_has_payload_slot_and_no_external_assets() {
        assert_eq!(TEMPLATE.matches("__PAYLOAD__").count(), 1);
        assert!(!TEMPLATE.contains("http://"));
        assert!(!TEMPLATE.contains("https://"));
    }
}
