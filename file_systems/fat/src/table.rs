//! Fixed-length array that stores only the chunks ever written.

use std::ops::{Index, IndexMut};

// Small enough that scattered files waste little; large enough that the chunk
// index stays tiny even at the cluster budget.
const CHUNK: usize = 4096;

/// Unwritten chunks read as `blank` and cost no memory, so a fresh volume is
/// nearly free and memory grows with the used part of the disk.
pub(crate) struct Table<T> {
    chunks: Vec<Option<Box<[T]>>>,
    len: usize,
    blank: T,
}

impl<T: Copy> Table<T> {
    pub(crate) fn new(len: usize, blank: T) -> Table<T> {
        Table {
            chunks: std::iter::repeat_with(|| None)
                .take(len.div_ceil(CHUNK))
                .collect(),
            len,
            blank,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn get(&self, i: usize) -> Option<T> {
        (i < self.len).then(|| self[i])
    }

    #[cfg(test)]
    fn chunks_in_use(&self) -> usize {
        self.chunks.iter().flatten().count()
    }
}

impl<T: Copy> Index<usize> for Table<T> {
    type Output = T;

    fn index(&self, i: usize) -> &T {
        assert!(
            i < self.len,
            "index {i} out of bounds for length {}",
            self.len
        );
        match &self.chunks[i / CHUNK] {
            Some(c) => &c[i % CHUNK],
            None => &self.blank,
        }
    }
}

impl<T: Copy> IndexMut<usize> for Table<T> {
    // A write allocates its chunk; a read never does.
    fn index_mut(&mut self, i: usize) -> &mut T {
        assert!(
            i < self.len,
            "index {i} out of bounds for length {}",
            self.len
        );
        let blank = self.blank;
        let c = self.chunks[i / CHUNK].get_or_insert_with(|| vec![blank; CHUNK].into());
        &mut c[i % CHUNK]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_free_writes_allocate_one_chunk() {
        let mut t = Table::new(10 * CHUNK + 1, 0u32);
        assert!((0..t.len()).all(|i| t[i] == 0));
        assert_eq!(t.chunks_in_use(), 0);

        t[CHUNK + 5] = 7;
        t[CHUNK + 6] = 8;
        assert_eq!(t.chunks_in_use(), 1);
        assert_eq!((t[CHUNK + 5], t[CHUNK + 6], t[CHUNK + 7]), (7, 8, 0));

        let last = t.len() - 1;
        t[last] = 9;
        assert_eq!(t.chunks_in_use(), 2);
        assert_eq!(t.get(last), Some(9));
        assert_eq!(t.get(t.len()), None);
    }

    #[test]
    #[should_panic]
    fn index_past_len_panics_like_a_vec() {
        let t = Table::new(CHUNK + 1, 0u32);
        let _ = t[CHUNK + 1]; // inside the last chunk's storage, still out of bounds
    }
}
