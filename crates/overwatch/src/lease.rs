/// Lease lifecycle management.
use anyhow::Result;

pub fn begin(key: &str, title: &str, session: Option<&str>) -> Result<()> {
    unimplemented!("lease::begin")
}

pub fn run(key: &str, note: Option<&str>) -> Result<()> {
    unimplemented!("lease::run")
}

pub fn end(key: &str, status: &str) -> Result<()> {
    unimplemented!("lease::end")
}

pub fn heartbeat(key: &str) -> Result<()> {
    unimplemented!("lease::heartbeat")
}

pub fn reap() -> Result<()> {
    unimplemented!("lease::reap")
}
