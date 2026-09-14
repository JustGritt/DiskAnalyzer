//! `report.json`: the report payload on its own, for scripts, for keeping a
//! history of scans, and for `--compare` to read back later.

use std::io::{self, Write};

use super::sink::OutputDir;
use crate::util::Json;

pub fn write_json(payload: &Json, out: &OutputDir) -> io::Result<()> {
    let mut file = out.create_file("report.json")?;
    file.write_all(payload.to_json_string().as_bytes())?;
    file.write_all(b"\n")?;
    file.flush()
}
