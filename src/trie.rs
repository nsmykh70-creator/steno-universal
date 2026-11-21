//! Байтовое префиксное дерево словаря.
//!
//! Хранит записи всех пяти слоёв; обход из произвольной позиции входа выдаёт
//! все совпадающие префиксы (не только длиннейший) — этого достаточно для
//! точного DP-кодировщика и жадного поиска эталона fixed-3B.
//!
//! Значение в узле упаковано как `(lane << 29) | id` (lane 0..4, id < 2^29);
//! `NO_ENTRY` означает «узел не является концом записи».

pub const NO_ENTRY: u32 = u32::MAX;

#[derive(Default)]
pub struct Trie {
    nodes: Vec<Node>,
}

#[derive(Default)]
struct Node {
    children: Vec<(u8, u32)>,
    entry: u32,
}

impl Node {
    fn leaf() -> Node {
        Node { children: Vec::new(), entry: NO_ENTRY }
    }
}

impl Trie {
    pub fn new() -> Trie {
        Trie { nodes: vec![Node::leaf()] }
    }

    pub fn clear(&mut self) {
        self.nodes.clear();
        self.nodes.push(Node::leaf());
    }

    /// Вставка записи; `entry` — упакованная пара (слой, ID).
    /// Паник невозможен: при переполнении узлов вставка игнорируется,
    /// вызывать до кодирования через `Dictionary::build` (там `Result`).
    pub fn insert(&mut self, bytes: &[u8], entry: u32) {
        if bytes.is_empty() || bytes.len() > 256 {
            return;
        }
        let mut node = 0u32;
        for &b in bytes {
            let cur = &self.nodes[node as usize];
            node = match cur.children.iter().find(|&&(cb, _)| cb == b) {
                Some(&(_, next)) => next,
                None => {
                    let next = self.nodes.len() as u32;
                    self.nodes.push(Node::leaf());
                    // повторно берём ссылку после push (borrows: используем индекс)
                    self.nodes[node as usize].children.push((b, next));
                    next
                }
            };
        }
        self.nodes[node as usize].entry = entry;
    }

    /// Все совпадения словаря, начинающиеся с `start`: пары (длина в байтах, entry).
    pub fn matches_at<'s, 'b>(&'s self, input: &'b [u8], start: usize) -> Matches<'s, 'b> {
        Matches { trie: self, input, node: 0, pos: start, start, live: true }
    }

    /// Самое длинное совпадение с позиции `start`.
    pub fn longest_at(&self, input: &[u8], start: usize) -> Option<(usize, u32)> {
        self.matches_at(input, start).last()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
}

/// Итератор всех словарных совпадений с фиксированной позиции.
pub struct Matches<'s, 'b> {
    trie: &'s Trie,
    input: &'b [u8],
    node: u32,
    pos: usize,
    start: usize,
    live: bool,
}

impl Iterator for Matches<'_, '_> {
    type Item = (usize, u32);

    fn next(&mut self) -> Option<(usize, u32)> {
        if !self.live {
            return None;
        }
        loop {
            if self.pos >= self.input.len() {
                self.live = false;
                return None;
            }
            let b = self.input[self.pos];
            let cur = &self.trie.nodes[self.node as usize];
            let next = match cur.children.iter().find(|&&(cb, _)| cb == b) {
                Some(&(_, n)) => n,
                None => {
                    self.live = false;
                    return None;
                }
            };
            self.node = next;
            self.pos += 1;
            let entry = self.trie.nodes[next as usize].entry;
            if entry != NO_ENTRY {
                return Some((self.pos - self.start, entry));
            }
        }
    }
}

/// Распаковка значения узла в пару (слой, ID слоя).
pub fn unpack_entry(entry: u32) -> (usize, u32) {
    ((entry >> 29) as usize, entry & 0x1FFF_FFFF)
}

/// Упаковка пары (слой, ID) в значение узла.
pub fn pack_entry(lane: usize, id: u32) -> u32 {
    ((lane as u32) << 29) | (id & 0x1FFF_FFFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_all_prefixes() {
        let mut t = Trie::new();
        t.insert(b"ab", pack_entry(1, 0));
        t.insert(b"abcd", pack_entry(3, 1));
        t.insert(b"abc", pack_entry(2, 2));
        let input = b"abcdef";
        let got: Vec<(usize, u32)> = t.matches_at(input, 0).collect();
        assert_eq!(got, vec![(2, pack_entry(1, 0)), (3, pack_entry(2, 2)), (4, pack_entry(3, 1))]);
        assert_eq!(t.longest_at(input, 0), Some((4, pack_entry(3, 1))));
        assert_eq!(t.matches_at(input, 4).next(), None);
    }

    #[test]
    fn unpack_roundtrip() {
        for lane in 0..5usize {
            for id in [0u32, 1, 32, 8191, 1_048_575] {
                let e = pack_entry(lane, id);
                assert_eq!(unpack_entry(e), (lane, id));
            }
        }
    }
}
