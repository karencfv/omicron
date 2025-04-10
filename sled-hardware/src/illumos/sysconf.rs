// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Access to sysconf-related info.

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("sysconf failed accessing {arg}: {e}")]
    Sysconf { arg: &'static str, e: std::io::Error },

    #[error("Integer conversion error: {0}")]
    Integer(#[from] std::num::TryFromIntError),
}

/// Returns the number of online processors on this sled.
pub fn online_processor_count() -> Result<u32, Error> {
    // Although the value returned by sysconf(3c) is an i64, we parse
    // the value as a u32.
    //
    // A value greater than u32::MAX (or a negative value) would return
    // an error here.
    let res = illumos_utils::libc::sysconf(libc::_SC_NPROCESSORS_ONLN)
        .map_err(|e| Error::Sysconf { arg: "online processor count", e })?;
    Ok(u32::try_from(res)?)
}

/// Returns the number of physical RAM pages on this sled.
pub fn usable_physical_pages() -> Result<u64, Error> {
    let pages = illumos_utils::libc::sysconf(libc::_SC_PHYS_PAGES)
        .map_err(|e| Error::Sysconf { arg: "physical pages", e })?
        .try_into()?;
    Ok(pages)
}

/// Returns the amount of RAM on this sled, in bytes.
pub fn usable_physical_ram_bytes() -> Result<u64, Error> {
    let page_size: u64 = illumos_utils::libc::sysconf(libc::_SC_PAGESIZE)
        .map_err(|e| Error::Sysconf { arg: "physical page size", e })?
        .try_into()?;

    // Note that `_SC_PHYS_PAGES` counts, specifically, the number of
    // `_SC_PAGESIZE` pages of physical memory. This means the multiplication
    // below yields the total physical RAM bytes, even if in some sense there
    // are fewer "actual" physical pages in page tables (such as if there were
    // 2MiB pages mixed in on x86).
    Ok(usable_physical_pages()? * page_size)
}
