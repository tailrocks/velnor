#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilerCacheBackend {
    Sccache,
    Kache,
    Off,
}
