pub struct BridgeCore;

#[boltffi::export]
impl BridgeCore {
    pub fn version() -> u32 {
        1
    }
}
