use std::{fmt::Debug, sync::Arc};

pub struct Entry<K, V> {
    key: Option<Arc<K>>,
    value: V,
    is_fresh: bool,
}

impl<K, V> Debug for Entry<K, V>
where
    K: Debug,
    V: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("key", self.key())
            .field("value", &self.value)
            .field("is_fresh", &self.is_fresh)
            .finish()
    }
}

impl<K, V> Entry<K, V> {
    pub(crate) fn new(key: Option<Arc<K>>, value: V, is_fresh: bool) -> Self {
        Self {
            key,
            value,
            is_fresh,
        }
    }

    pub fn key(&self) -> &K {
        self.key.as_ref().expect("Bug: Key is None")
    }

    pub fn value(&self) -> &V {
        &self.value
    }

    pub fn into_value(self) -> V {
        self.value
    }

    pub fn is_fresh(&self) -> bool {
        self.is_fresh
    }
}
