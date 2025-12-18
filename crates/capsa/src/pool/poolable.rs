use std::marker::PhantomData;

pub struct Yes;
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
