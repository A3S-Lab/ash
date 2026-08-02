// ASH_FAULT cache-miss
pub fn load() -> Result<(), &'static str> {
    Err("E_CACHE")
}
