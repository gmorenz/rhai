use crate::stdlib::{
    collections::HashMap,
    mem::{replace, transmute},    
    num::NonZeroU32,
    sync::RwLock,
};

/* A note on potential performance improvements

There are two obvious potential uses of unsafe here that might lead to
a substantial benefit if this becomes a hotspot.

The first is replacing `enum Entry` with `union Entry`. Already every time
we access `Entry` we know which type it is ahead of time, so it would be a
simple matter that might the number of branches. Hopefully using NonZeroU32
means that using an enum hasn't bloated the size.

The second is replacing `data: String` with `data: Box<[u8]>`, and using unsafe
from_utf8_unchecked methods when moving back to a str type. This would reduce
the size of the entry struct by a pointer (1/3rd it's size).

*/

// Keywords, keep in sync with Interner::new
// Safety: We never put a 0 inside new_unchecked...
pub const KEYWORD_PRINT: StaticStr = StaticStr(unsafe{ NonZeroU32::new_unchecked(1) }); // "print";
pub const KEYWORD_DEBUG: StaticStr = StaticStr(unsafe{ NonZeroU32::new_unchecked(2) }); // "debug";
pub const KEYWORD_TYPE_OF: StaticStr = StaticStr(unsafe{ NonZeroU32::new_unchecked(3) }); // "type_of";
pub const KEYWORD_EVAL: StaticStr = StaticStr(unsafe{ NonZeroU32::new_unchecked(4) }); // "eval";
pub const KEYWORD_TRUE: StaticStr = StaticStr(unsafe{ NonZeroU32::new_unchecked(5) }); // "true"
pub const KEYWORD_FALSE: StaticStr = StaticStr(unsafe{ NonZeroU32::new_unchecked(6) }); // "false"
pub const KEYWORD_LET: StaticStr = StaticStr(unsafe{ NonZeroU32::new_unchecked(7) }); // "let"
pub const KEYWORD_CONST: StaticStr = StaticStr(unsafe{ NonZeroU32::new_unchecked(8) }); // "const"
pub const KEYWORD_IF: StaticStr = StaticStr(unsafe{ NonZeroU32::new_unchecked(9) }); // "if"
pub const KEYWORD_ELSE: StaticStr = StaticStr(unsafe{ NonZeroU32::new_unchecked(10) }); // "else"
pub const KEYWORD_WHILE: StaticStr = StaticStr(unsafe{ NonZeroU32::new_unchecked(11) }); // "while"
pub const KEYWORD_LOOP: StaticStr = StaticStr(unsafe{ NonZeroU32::new_unchecked(12) }); // "loop"
pub const KEYWORD_CONTINUE: StaticStr = StaticStr(unsafe{ NonZeroU32::new_unchecked(13) }); // "continue"
pub const KEYWORD_BREAK: StaticStr = StaticStr(unsafe{ NonZeroU32::new_unchecked(14) }); // "break"
pub const KEYWORD_RETURN: StaticStr = StaticStr(unsafe{ NonZeroU32::new_unchecked(15) }); // "return"
pub const KEYWORD_THROW: StaticStr = StaticStr(unsafe{ NonZeroU32::new_unchecked(16) }); // "throw"
pub const KEYWORD_FOR: StaticStr = StaticStr(unsafe{ NonZeroU32::new_unchecked(17) }); // "for"
pub const KEYWORD_IN: StaticStr = StaticStr(unsafe{ NonZeroU32::new_unchecked(18) }); // "in"
pub const KEYWORD_FN: StaticStr = StaticStr(unsafe{ NonZeroU32::new_unchecked(19) }); // "fn"

#[derive(PartialEq, Eq, Hash)]
pub struct Str(NonZeroU32);

/// Like Str but without a `Drop`. This is a workaround to allow fast pattern matching.
/// StaticStr's should not be retained.
#[derive(PartialEq, Eq)]
pub struct StaticStr(NonZeroU32);

impl PartialEq<Str> for StaticStr {
    fn eq(&self, other: &Str) -> bool { self.0 == other.0 }
}

impl PartialEq<StaticStr> for Str {
fn eq(&self, other: &StaticStr) -> bool { self.0 == other.0 }
    
}

pub fn intern_string(string: String) -> Str {
    let mut interner = INTERNER.write().unwrap();
    if let Some(&idx) = interner.dedup_map.get(&string) {
        match &mut interner.strings[idx.get() as usize] {
            Entry::Occupied{ref mut refs, ..} => { 
                println!("{} Reintern {} at {}", idx, string, refs.get());
                *refs = NonZeroU32::new(refs.get() + 1).unwrap();
                println!("{} Reintern {} to {}", idx, string, refs.get());
            }
            Entry::Vacant(..) => unreachable!()
        };
        return Str(idx);
    }

    let entry = Entry::Occupied{
        refs: NonZeroU32::new(1).unwrap(),
        data: string.clone() ,
    };

    if let Some(vacancy) = interner.vacant_head {
        println!("{} New Vacant {}", vacancy, string);
        interner.dedup_map.insert(string, vacancy);
        let entry = replace(&mut interner.strings[vacancy.get() as usize], entry);
        match entry {
            Entry::Vacant(next_vacancy) => interner.vacant_head = next_vacancy,
            Entry::Occupied{..} => unreachable!(),
        }
        Str(vacancy)
    }
    else {
        let next = NonZeroU32::new(interner.strings.len() as u32).unwrap();
        println!("{} New Tail {}", next, string);
        interner.dedup_map.insert(string, next);
        interner.strings.push(entry);
        Str(next)
    }
}

impl Str {
    pub fn static_str(&self) -> StaticStr {
        StaticStr(self.0)
    }

    pub fn get_string(&self) -> String {
        self.get_str().to_owned()
    }

    pub fn get_str<'a>(&'a self) -> &'a str {
        let interner = INTERNER.read().unwrap();

        let entry = &interner.strings[self.0.get() as usize];
        let data = match entry {
            Entry::Occupied{ data, .. } => data,
            Entry::Vacant(..) => unreachable!(),
        };

        // Safety:
        // 
        // Data in an entry isn't touched until the ref count is zero,
        // as long as self exists the ref count will not be zero, so the data
        // pointed to by data is never touched.
        // 
        // Note that moving a String (which happens when the containing vec is
        // reallocated) does not move the data the string points to.
        unsafe {
            transmute::<&str, &'a str>(data)
        }
        
    }
}

impl Clone for Str {
    fn clone(&self) -> Str {
        let mut interner = INTERNER.write().unwrap();
        match &mut interner.strings[self.0.get() as usize] {
            &mut Entry::Occupied{ ref mut refs, ref data } => {
                *refs = NonZeroU32::new(refs.get() + 1).unwrap();
                println!("{} Clone {} to {}", self.0, data, refs.get());
            }
            _ => unreachable!()
        }
        Str(self.0)
    }
}

impl AsRef<str> for Str {
    fn as_ref(&self) -> &str { self.get_str() }
}

impl Into<String> for Str {
    fn into(self) -> String { self.get_string() }
}

impl From<String> for Str {
    fn from(string: String) -> Self { intern_string(string) }
}

impl From<StaticStr> for Str {
    fn from(str: StaticStr) -> Self {
        let s = Str(str.0);
        let r = s.clone();
        std::mem::forget(s);
        r
    }
}

impl <'a> From<&'a str> for Str {
    // TODO: Performance: Could make an intern_str function that only allocates
    // if the str is not found. Possibly could make really use of generics to avoid
    // code duplication.
    fn from(str: &'a str) -> Self { intern_string(str.to_owned()) }
}

impl From<char> for Str {
    // TODO: Performance: Could make an intern_str function that only allocates
    // if the str is not found. Possibly could make really use of generics to avoid
    // code duplication.
    fn from(c: char) -> Self { intern_string(c.to_string()) }
}

impl std::fmt::Display for Str {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { 
        self.get_str().fmt(f)
    }
}

impl std::fmt::Debug for Str {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { 
        self.get_str().fmt(f)
    }
}

impl Drop for Str {
    fn drop(&mut self) {
        let mut interner = INTERNER.write().unwrap();
        let idx = self.0.get() as usize;
        match &mut interner.strings[idx] {
            &mut Entry::Occupied{ ref mut refs, ref data } => {
                let v = refs.get();
                if v > 1 {
                    *refs = NonZeroU32::new(v - 1).unwrap();
                    println!("{} Decr {} to {}", self.0, data, refs.get());
                    return;
                }
                else {
                    println!("{} Free {}", self.0, data);
                }
            }
            _ => unreachable!("{} Free Vacant", self.0),
        }

        assert!(idx > 19, "Trying to free keyword: {}", idx);
        let new_entry = Entry::Vacant(interner.vacant_head);
        // We only reach here if refs should now be 0
        let old_entry = replace(&mut interner.strings[idx], new_entry);
        
        match old_entry {
            Entry::Occupied{ data, .. } => interner.dedup_map.remove(&data),
            _ => unreachable!(),
        };
        
        interner.vacant_head = Some(self.0);
    }
}

lazy_static! {
    static ref INTERNER: RwLock<Interner> = RwLock::new(Interner::new());
}

enum Entry {
    Occupied{ refs: NonZeroU32, data: String },
    Vacant(Option<NonZeroU32>),
}

struct StoredString {
    refs: NonZeroU32,
    data: String,
}

struct Interner {
    next_idx: NonZeroU32,
    strings: Vec<Entry>,
    vacant_head: Option<NonZeroU32>,
    // TODO: Don't duplicate strings here
    dedup_map: HashMap<String, NonZeroU32>,
}


impl Interner {
    fn new() -> Self {
        // First entry is a dummy entry to allow indexing without offsets
        // Then we have the list of hardcoded keywords
        let strings = vec![
            Entry::Vacant(None),
            Entry::Occupied{ refs: NonZeroU32::new(1).unwrap(), data: "print".to_owned() },    
            Entry::Occupied{ refs: NonZeroU32::new(1).unwrap(), data: "debug".to_owned() },
            Entry::Occupied{ refs: NonZeroU32::new(1).unwrap(), data: "type_of".to_owned() },
            Entry::Occupied{ refs: NonZeroU32::new(1).unwrap(), data: "eval".to_owned() },
            Entry::Occupied{ refs: NonZeroU32::new(1).unwrap(), data: "true".to_owned() },
            Entry::Occupied{ refs: NonZeroU32::new(1).unwrap(), data: "false".to_owned() },
            Entry::Occupied{ refs: NonZeroU32::new(1).unwrap(), data: "let".to_owned() },
            Entry::Occupied{ refs: NonZeroU32::new(1).unwrap(), data: "const".to_owned() },
            Entry::Occupied{ refs: NonZeroU32::new(1).unwrap(), data: "if".to_owned() },
            Entry::Occupied{ refs: NonZeroU32::new(1).unwrap(), data: "else".to_owned() },
            Entry::Occupied{ refs: NonZeroU32::new(1).unwrap(), data: "while".to_owned() },
            Entry::Occupied{ refs: NonZeroU32::new(1).unwrap(), data: "loop".to_owned() },
            Entry::Occupied{ refs: NonZeroU32::new(1).unwrap(), data: "continue".to_owned() },
            Entry::Occupied{ refs: NonZeroU32::new(1).unwrap(), data: "break".to_owned() },
            Entry::Occupied{ refs: NonZeroU32::new(1).unwrap(), data: "return".to_owned() },
            Entry::Occupied{ refs: NonZeroU32::new(1).unwrap(), data: "throw".to_owned() },
            Entry::Occupied{ refs: NonZeroU32::new(1).unwrap(), data: "for".to_owned() },
            Entry::Occupied{ refs: NonZeroU32::new(1).unwrap(), data: "in".to_owned() },
            Entry::Occupied{ refs: NonZeroU32::new(1).unwrap(), data: "fn".to_owned() },
        ];
        Interner {
            next_idx: NonZeroU32::new(strings.len() as u32).unwrap(),
            strings,
            vacant_head: None,
            dedup_map: HashMap::new(),
        }
    }
}