use std::marker::PhantomData;

/// Marker for pool mode - `Capsa::pool()` returns this.
pub struct Yes;

/// Marker for single VM mode - `Capsa::vm()` returns this.
pub struct No;

pub struct Poolability<P>(PhantomData<P>);

impl<P> Poolability<P> {
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

impl<P> Default for Poolability<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P> Clone for Poolability<P> {
    fn clone(&self) -> Self {
        Self::new()
    }
}
