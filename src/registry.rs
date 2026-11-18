use std::{io, ptr};
use winapi::{
    shared::{
        minwindef::HKEY,
        winerror::{
            ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS,
            ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
        },
    },
    um::{
        stringapiset::CompareStringOrdinal,
        winnt::{KEY_QUERY_VALUE, KEY_SET_VALUE, REG_EXPAND_SZ, REG_SZ},
        winreg::{
            RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegEnumValueW, RegOpenKeyExW,
            RegQueryValueExW, RegSetValueExW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
        },
        winuser::{SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE},
    },
};

type Result<T> = std::result::Result<T, Error>;
const MAX_NAME: usize = 16_383;
const MAX_VALUE_UNITS: usize = 32_767;
const MAX_READ_BYTES: u32 = 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    System,
}

impl Scope {
    pub fn store(self) -> Store<'static> {
        match self {
            Self::User => Store {
                root: HKEY_CURRENT_USER,
                path: "Environment",
                create: true,
            },
            Self::System => Store {
                root: HKEY_LOCAL_MACHINE,
                path: "SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment",
                create: false,
            },
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Windows(u32),
    Conflict,
    Exists,
    Invalid(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Windows(code) => {
                write!(formatter, "{}", io::Error::from_raw_os_error(code as i32))
            }
            Self::Conflict => {
                formatter.write_str("Variable changed outside ee. Refresh and reopen it.")
            }
            Self::Exists => formatter.write_str("That name already exists in this scope."),
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl Error {
    pub fn access_denied(&self) -> bool {
        matches!(self, Self::Windows(ERROR_ACCESS_DENIED))
    }
}

#[derive(Clone)]
pub struct Name {
    units: Vec<u16>,
    pub text: String,
}

impl Name {
    fn from_units(units: Vec<u16>) -> Self {
        let text = String::from_utf16_lossy(&units);
        Self { units, text }
    }

    pub fn new(text: &str) -> Result<Self> {
        let units: Vec<_> = text.encode_utf16().collect();
        if units.is_empty()
            || units.len() > MAX_NAME
            || text.contains('=')
            || text.chars().any(char::is_control)
        {
            return Err(Error::Invalid(
                "Name is empty, too long, or contains '=' or a control character.",
            ));
        }
        Ok(Self {
            units,
            text: text.to_owned(),
        })
    }

    fn terminated(&self) -> Vec<u16> {
        self.units.iter().copied().chain([0]).collect()
    }

    pub fn matches(&self, other: &Self) -> bool {
        ordinal(&self.units, &other.units).is_eq()
    }

    pub fn contains(&self, query: &[u16]) -> bool {
        query.is_empty()
            || self
                .units
                .windows(query.len())
                .any(|part| ordinal(part, query).is_eq())
    }

    pub fn is_path(&self) -> bool {
        const PATH: &[u16] = &[b'P' as u16, b'a' as u16, b't' as u16, b'h' as u16];
        ordinal(&self.units, PATH).is_eq()
    }

    pub fn editable(&self) -> bool {
        !self.units.is_empty() && !self.units.contains(&(b'=' as u16)) && !self.units.contains(&0)
    }
}

fn ordinal(left: &[u16], right: &[u16]) -> std::cmp::Ordering {
    if left.is_empty() || right.is_empty() {
        return left.len().cmp(&right.len());
    }
    let result = unsafe {
        CompareStringOrdinal(
            left.as_ptr(),
            left.len() as i32,
            right.as_ptr(),
            right.len() as i32,
            1,
        )
    };
    if result == 0 {
        left.cmp(right)
    } else {
        result.cmp(&2)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Plain,
    Expandable,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Plain => "REG_SZ",
            Self::Expandable => "REG_EXPAND_SZ",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Raw {
    pub kind: u32,
    bytes: Vec<u8>,
}

impl Raw {
    fn encode(kind: Kind, text: &str) -> Result<Self> {
        if text.contains('\0') || text.encode_utf16().count() >= MAX_VALUE_UNITS {
            return Err(Error::Invalid(
                "Value contains NUL or exceeds 32,766 UTF-16 units.",
            ));
        }
        Ok(Self {
            kind: match kind {
                Kind::Plain => REG_SZ,
                Kind::Expandable => REG_EXPAND_SZ,
            },
            bytes: text
                .encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect(),
        })
    }

    pub fn decode(&self) -> Option<(Kind, String)> {
        let kind = match self.kind {
            REG_SZ => Kind::Plain,
            REG_EXPAND_SZ => Kind::Expandable,
            _ => return None,
        };
        let (pairs, trailing) = self.bytes.as_chunks::<2>();
        if !trailing.is_empty() {
            return None;
        }
        let mut units: Vec<_> = pairs.iter().copied().map(u16::from_le_bytes).collect();
        if units.last() == Some(&0) {
            units.pop();
        }
        if units.contains(&0) {
            return None;
        }
        Some((kind, String::from_utf16(&units).ok()?))
    }
}

pub struct Entry {
    pub name: Name,
    pub raw: Raw,
}

pub struct Store<'a> {
    root: HKEY,
    path: &'a str,
    create: bool,
}

impl Store<'_> {
    fn open(&self, access: u32, create: bool) -> Result<Option<Key>> {
        let mut handle = ptr::null_mut();
        let path: Vec<_> = self.path.encode_utf16().chain([0]).collect();
        let code =
            unsafe { RegOpenKeyExW(self.root, path.as_ptr(), 0, access, &mut handle) } as u32;
        match code {
            ERROR_SUCCESS => Ok(Some(Key(handle))),
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND if self.create && create => {
                status(unsafe {
                    RegCreateKeyExW(
                        self.root,
                        path.as_ptr(),
                        0,
                        ptr::null_mut(),
                        0,
                        access,
                        ptr::null_mut(),
                        &mut handle,
                        ptr::null_mut(),
                    )
                })?;
                Ok(Some(Key(handle)))
            }
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND if self.create => Ok(None),
            _ => Err(Error::Windows(code)),
        }
    }

    pub fn names(&self) -> Result<Vec<Name>> {
        let Some(key) = self.open(KEY_QUERY_VALUE, false)? else {
            return Ok(Vec::new());
        };
        let mut names = Vec::new();
        let mut buffer = vec![0; MAX_NAME + 1];
        for index in 0.. {
            let mut len = buffer.len() as u32;
            let code = unsafe {
                RegEnumValueW(
                    key.0,
                    index,
                    buffer.as_mut_ptr(),
                    &mut len,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            };
            if code as u32 == ERROR_NO_MORE_ITEMS {
                break;
            }
            status(code)?;
            names.push(Name::from_units(buffer[..len as usize].to_vec()));
        }
        names.sort_unstable_by(|left, right| ordinal(&left.units, &right.units));
        names.dedup_by(|left, right| left.matches(right));
        Ok(names)
    }

    pub fn read(&self, name: Name) -> Result<Entry> {
        let key = self.open(KEY_QUERY_VALUE, false)?.ok_or(Error::Conflict)?;
        let raw = key.read(&name.terminated())?.ok_or(Error::Conflict)?;
        Ok(Entry { name, raw })
    }

    pub fn save(
        &self,
        name: &Name,
        expected: Option<&Raw>,
        kind: Kind,
        text: &str,
    ) -> Result<bool> {
        if !name.editable() {
            return Err(Error::Invalid(
                "This registry name is not an editable environment variable.",
            ));
        }
        let unchanged = match expected {
            Some(raw) => match raw.decode() {
                Some((original_kind, original_text)) if original_kind == kind => {
                    original_text == text
                }
                _ => {
                    return Err(Error::Invalid(
                        "Only valid string values can be edited, without changing their type.",
                    ))
                }
            },
            None => false,
        };
        let replacement = if unchanged {
            None
        } else {
            Some(Raw::encode(kind, text)?)
        };
        let access = KEY_QUERY_VALUE | if unchanged { 0 } else { KEY_SET_VALUE };
        let key = self
            .open(access, expected.is_none())?
            .ok_or(Error::Conflict)?;
        let name = name.terminated();
        key.check(&name, expected)?;
        if let Some(replacement) = replacement {
            key.write(&name, &replacement)?;
        }
        Ok(!unchanged)
    }

    pub fn delete(&self, entry: &Entry) -> Result<()> {
        if !entry.name.editable() || entry.raw.decode().is_none() {
            return Err(Error::Invalid("This registry entry is read-only."));
        }
        let key = self
            .open(KEY_QUERY_VALUE | KEY_SET_VALUE, false)?
            .ok_or(Error::Conflict)?;
        let name = entry.name.terminated();
        key.check(&name, Some(&entry.raw))?;
        status(unsafe { RegDeleteValueW(key.0, name.as_ptr()) })
    }
}

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

impl Key {
    fn read(&self, name: &[u16]) -> Result<Option<Raw>> {
        let mut len = 0;
        let mut kind = 0;
        let mut bytes = Vec::new();
        for _ in 0..6 {
            let code = unsafe {
                RegQueryValueExW(
                    self.0,
                    name.as_ptr(),
                    ptr::null_mut(),
                    &mut kind,
                    if bytes.is_empty() {
                        ptr::null_mut()
                    } else {
                        bytes.as_mut_ptr()
                    },
                    &mut len,
                )
            };
            match code as u32 {
                ERROR_FILE_NOT_FOUND => return Ok(None),
                ERROR_SUCCESS if len as usize <= bytes.len() => {
                    bytes.truncate(len as usize);
                    return Ok(Some(Raw { kind, bytes }));
                }
                ERROR_SUCCESS | ERROR_MORE_DATA if len <= MAX_READ_BYTES => {
                    bytes.resize(len as usize, 0)
                }
                ERROR_SUCCESS | ERROR_MORE_DATA => {
                    return Err(Error::Invalid("Registry value is too large to open in ee."))
                }
                _ => return Err(Error::Windows(code as u32)),
            }
        }
        Err(Error::Conflict)
    }

    fn check(&self, name: &[u16], expected: Option<&Raw>) -> Result<()> {
        match (expected, self.read(name)?.as_ref()) {
            (None, Some(_)) => Err(Error::Exists),
            (expected, actual) if expected == actual => Ok(()),
            _ => Err(Error::Conflict),
        }
    }

    fn write(&self, name: &[u16], raw: &Raw) -> Result<()> {
        status(unsafe {
            RegSetValueExW(
                self.0,
                name.as_ptr(),
                0,
                raw.kind,
                raw.bytes.as_ptr(),
                raw.bytes.len() as u32,
            )
        })
    }
}

fn status(code: i32) -> Result<()> {
    match code as u32 {
        ERROR_SUCCESS => Ok(()),
        code => Err(Error::Windows(code)),
    }
}

pub fn notify() -> bool {
    let environment: Vec<_> = "Environment".encode_utf16().chain([0]).collect();
    let mut result = 0;
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            environment.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            200,
            &mut result,
        ) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use winapi::um::{winnt::REG_BINARY, winreg::RegDeleteTreeW};

    struct TemporaryKey(String);

    impl TemporaryKey {
        fn new() -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(format!("Software\\ee-test-{}-{stamp}", std::process::id()))
        }

        fn store(&self) -> Store<'_> {
            Store {
                root: HKEY_CURRENT_USER,
                path: &self.0,
                create: true,
            }
        }
    }

    impl Drop for TemporaryKey {
        fn drop(&mut self) {
            let path: Vec<_> = self.0.encode_utf16().chain([0]).collect();
            unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, path.as_ptr()) };
        }
    }

    #[test]
    fn registry_crud_and_conflicts() {
        let temp = TemporaryKey::new();
        let store = temp.store();
        let name = Name::new("EE_测试").unwrap();
        let lower = Name::new("ee_测试").unwrap();
        assert!(store.names().unwrap().is_empty());
        assert!(store
            .save(&name, None, Kind::Expandable, "%TEMP%\\测试;😀")
            .unwrap());
        assert!(matches!(
            store.save(&lower, None, Kind::Plain, "new"),
            Err(Error::Exists)
        ));
        assert_eq!(store.names().unwrap().len(), 1);
        let original = store.read(name.clone()).unwrap();
        assert_eq!(
            original.raw.decode(),
            Some((Kind::Expandable, "%TEMP%\\测试;😀".into()))
        );
        assert!(!store
            .save(
                &name,
                Some(&original.raw),
                Kind::Expandable,
                "%TEMP%\\测试;😀"
            )
            .unwrap());
        assert!(store
            .save(&name, Some(&original.raw), Kind::Expandable, "")
            .unwrap());
        assert!(matches!(store.delete(&original), Err(Error::Conflict)));
        assert!(matches!(
            store.save(&name, Some(&original.raw), Kind::Expandable, "stale"),
            Err(Error::Conflict)
        ));
        let empty = store.read(lower).unwrap();
        assert_eq!(empty.raw.decode(), Some((Kind::Expandable, String::new())));
        store.delete(&empty).unwrap();
        assert!(store.names().unwrap().is_empty());
        assert!(matches!(store.delete(&empty), Err(Error::Conflict)));
    }

    #[test]
    fn unreadable_strings_and_other_types_are_read_only() {
        let temp = TemporaryKey::new();
        let store = temp.store();
        let name = Name::new("readOnly").unwrap();
        let key = store.open(KEY_SET_VALUE, true).unwrap().unwrap();
        for raw in [
            Raw {
                kind: REG_BINARY,
                bytes: vec![1, 0],
            },
            Raw {
                kind: REG_SZ,
                bytes: vec![0, 216, 0, 0],
            },
        ] {
            key.write(&name.terminated(), &raw).unwrap();
            let entry = store.read(name.clone()).unwrap();
            assert_eq!(entry.raw, raw);
            assert!(entry.raw.decode().is_none());
            assert!(matches!(store.delete(&entry), Err(Error::Invalid(_))));
            assert!(matches!(
                store.save(&name, Some(&entry.raw), Kind::Plain, "replacement"),
                Err(Error::Invalid(_))
            ));
        }
    }

    #[test]
    fn validates_names_and_searches_case_insensitively() {
        let name = Name::new("My_ÄÖ_😀").unwrap();
        assert!(name.contains(&"äö_😀".encode_utf16().collect::<Vec<_>>()));
        assert!(Name::new("pAtH").unwrap().is_path());
        for invalid in ["", "x=y", "line\n", "null\0"] {
            assert!(Name::new(invalid).is_err());
        }
        assert!(Raw::encode(Kind::Plain, "\0").is_err());
        assert!(Raw::encode(Kind::Plain, &"x".repeat(MAX_VALUE_UNITS)).is_err());
    }
}
