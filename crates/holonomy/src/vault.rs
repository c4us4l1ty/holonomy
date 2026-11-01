//! The desktop's encrypted document vault.
//!
//! A container payload holds **all documents belonging to one passphrase**, not one file per
//! document. Titles, document order and HDOC payloads are inside the AEAD-encrypted bytes. The
//! fixed-size master record therefore reveals neither how many notes exist nor which password was
//! used. Legacy single-document HDOC/raw-text payloads are read and upgraded on the next save.
//!
//! This module is desktop-only. The sealed boot keeps its existing pre-opened-fd commit path.
use std::fmt;
use std::path::{Path, PathBuf};

use holonomy_container::{Wavefunction, S_MAX_PAYLOAD};
use holonomy_text::Editor;
use zeroize::Zeroize;

use crate::session::{DocumentSnapshot, Session};

const MAGIC: &[u8; 8] = b"HVAULT1\0";
const MAX_DOCS: usize = 4096;
const MAX_TITLE: usize = 128;

#[derive(Debug)]
pub enum VaultError {
    Container(holonomy_container::ContainerError),
    Editor(holonomy_text::EditorError),
    InvalidBundle(&'static str),
    Exists,
    EmptyPassphrase,
    PassphrasesMatch,
    ConfirmationMismatch,
    InvalidTitle,
    TooLarge,
}
impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Container(e) => write!(f, "{e}"),
            Self::Editor(e) => write!(f, "{e}"),
            Self::InvalidBundle(s) => write!(f, "invalid document vault: {s}"),
            Self::Exists => write!(f, "that file already exists; choose Open instead"),
            Self::EmptyPassphrase => write!(f, "enter a passphrase"),
            Self::PassphrasesMatch => write!(f, "the two passphrases must be different"),
            Self::ConfirmationMismatch => write!(f, "the passphrases do not match"),
            Self::InvalidTitle => write!(f, "use a title of 1–128 characters, without control characters"),
            Self::TooLarge => write!(f, "this vault has reached its 8 MiB encrypted payload limit"),
        }
    }
}
impl std::error::Error for VaultError {}
impl From<holonomy_container::ContainerError> for VaultError {
    fn from(e: holonomy_container::ContainerError) -> Self { Self::Container(e) }
}
impl From<holonomy_text::EditorError> for VaultError {
    fn from(e: holonomy_text::EditorError) -> Self { Self::Editor(e) }
}

#[derive(Clone, Zeroize)]
#[zeroize(drop)]
pub struct StoredDocument {
    pub id: u64,
    pub title: String,
    pub payload: Vec<u8>,
    pub caret: u32,
}
impl fmt::Debug for StoredDocument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredDocument").field("id", &self.id).field("title", &self.title).finish_non_exhaustive()
    }
}

/// One mounted payload. Dropping it scrubs its keys and the decrypted bundle copy.
pub struct Vault {
    path: PathBuf,
    container: Wavefunction,
    documents: Vec<StoredDocument>,
    next_id: u64,
}
impl Vault {
    pub fn path(&self) -> &Path { &self.path }
    pub fn documents(&self) -> &[StoredDocument] { &self.documents }
    pub fn next_id(&mut self) -> u64 { let id = self.next_id; self.next_id += 1; id }

    /// Create a new file without ever adopting or truncating an unrelated existing file.
    pub fn create(path: &Path, primary: &str, primary_confirm: &str, decoy: &str, decoy_confirm: &str, vdf: u64) -> Result<(), VaultError> {
        for (a, b) in [(primary, primary_confirm), (decoy, decoy_confirm)] {
            if a.is_empty() { return Err(VaultError::EmptyPassphrase); }
            if a != b { return Err(VaultError::ConfirmationMismatch); }
        }
        if primary == decoy { return Err(VaultError::PassphrasesMatch); }
        let initial = encode_bundle(2, &[StoredDocument { id: 1, title: "Untitled".into(), payload: Editor::new().payload().map_err(VaultError::Editor)?, caret: 0 }])?;
        // Reserve exclusively before the container layer opens the path. A failed setup removes only
        // a file this call created, never a pre-existing user file.
        {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)
                .map_err(|e| if e.kind() == std::io::ErrorKind::AlreadyExists { VaultError::Exists } else { VaultError::Container(e.into()) })?;
        }
        let result = (|| {
            // Ω is uniformly random. Two small initial payloads can still collide; retry with a
            // fresh salt rather than presenting a rare, unactionable setup failure to the user.
            for attempt in 0..16 {
                match Wavefunction::create_with_duress(path, "Documents", &initial, primary, decoy, vdf) {
                    Ok(_) => return Ok(()),
                    Err(holonomy_container::ContainerError::DuressOverlap { .. }) if attempt < 15 => continue,
                    Err(e) => return Err(e.into()),
                }
            }
            unreachable!()
        })();
        if result.is_err() { let _ = std::fs::remove_file(path); }
        result
    }

    pub fn open(path: &Path, passphrase: &str, vdf: u64) -> Result<Self, VaultError> {
        if passphrase.is_empty() { return Err(VaultError::EmptyPassphrase); }
        let container = Wavefunction::open(path, "Documents", passphrase, vdf)?;
        let mut bytes = vec![0u8; container.content_len() as usize];
        container.read_content(0, &mut bytes)?;
        let (documents, next_id) = if bytes.starts_with(MAGIC) {
            decode_bundle(&bytes)?
        } else {
            let editor = legacy_editor(&bytes)?;
            let caret = editor.caret();
            let payload = editor.payload().map_err(VaultError::Editor)?;
            (vec![StoredDocument { id: 1, title: container.title().to_string(), payload, caret }], 2)
        };
        bytes.zeroize();
        Ok(Self { path: path.to_path_buf(), container, documents, next_id })
    }

    /// Merge resident edits, retaining documents that were closed but not deleted.
    pub fn save(&mut self, session: &mut Session) -> Result<(), VaultError> {
        for snap in session.document_snapshots().map_err(VaultError::Editor)? {
            let doc = StoredDocument { id: snap.id, title: snap.title, payload: snap.payload, caret: snap.caret };
            if let Some(old) = self.documents.iter_mut().find(|d| d.id == doc.id) { *old = doc; }
            else { self.documents.push(doc); }
        }
        let mut bytes = encode_bundle(self.next_id, &self.documents)?;
        let result = self.container.write_content(&bytes).map_err(VaultError::Container);
        bytes.zeroize();
        result?;
        session.mark_documents_saved();
        Ok(())
    }

    /// Persist the smaller bundle *before* removing the resident document. A failed write leaves
    /// the editor intact. `write_content` re-chaffs the freed tail, so deletion is not merely a
    /// sidebar removal or a rename to a hidden cleartext file.
    pub fn delete(&mut self, session: &mut Session, id: u64) -> Result<(), VaultError> {
        let mut remaining = self.documents.clone();
        remaining.retain(|d| d.id != id);
        for snap in session.document_snapshots().map_err(VaultError::Editor)? {
            if snap.id == id { continue; }
            let doc = StoredDocument { id: snap.id, title: snap.title, payload: snap.payload, caret: snap.caret };
            if let Some(old) = remaining.iter_mut().find(|d| d.id == doc.id) { *old = doc; } else { remaining.push(doc); }
        }
        let mut bytes = encode_bundle(self.next_id, &remaining)?;
        let result = self.container.write_content(&bytes).map_err(VaultError::Container);
        bytes.zeroize();
        result?;
        self.documents = remaining;
        session.remove_document(id);
        session.mark_documents_saved();
        Ok(())
    }

    pub fn load_all(&self, session: &mut Session) -> Result<(), VaultError> {
        let docs = self.documents.iter().map(|d| DocumentSnapshot {
            id: d.id, title: d.title.clone(), payload: d.payload.clone(), caret: d.caret,
            dirty: false, font_index: 0, style_index: 0, mode: 0,
        }).collect();
        session.replace_documents(docs).map_err(|_| VaultError::InvalidBundle("could not load documents"))?;
        session.clear_image_cache();
        Ok(())
    }
}

fn legacy_editor(bytes: &[u8]) -> Result<Editor, VaultError> {
    if bytes.starts_with(b"HDOC") { return Editor::from_payload(bytes).map_err(Into::into); }
    let text = std::str::from_utf8(bytes).map_err(|_| VaultError::InvalidBundle("legacy payload is neither HDOC nor UTF-8"))?;
    Editor::from_text(text).map_err(Into::into)
}

fn put_u16(out: &mut Vec<u8>, n: u16) { out.extend_from_slice(&n.to_le_bytes()); }
fn put_u32(out: &mut Vec<u8>, n: u32) { out.extend_from_slice(&n.to_le_bytes()); }
fn put_u64(out: &mut Vec<u8>, n: u64) { out.extend_from_slice(&n.to_le_bytes()); }
fn valid_title(s: &str) -> bool { !s.is_empty() && s.chars().count() <= MAX_TITLE && !s.chars().any(char::is_control) }

pub fn encode_bundle(next_id: u64, docs: &[StoredDocument]) -> Result<Vec<u8>, VaultError> {
    if docs.len() > MAX_DOCS || next_id == 0 { return Err(VaultError::InvalidBundle("document count/id")); }
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC); put_u64(&mut out, next_id); put_u32(&mut out, docs.len() as u32);
    let mut ids = std::collections::BTreeSet::new();
    for d in docs {
        if d.id == 0 || d.id >= next_id || !ids.insert(d.id) || !valid_title(&d.title) { return Err(VaultError::InvalidBundle("document id/title")); }
        if d.payload.len() > S_MAX_PAYLOAD as usize { return Err(VaultError::TooLarge); }
        put_u64(&mut out, d.id); put_u16(&mut out, d.title.len() as u16); put_u32(&mut out, d.payload.len() as u32); put_u32(&mut out, d.caret);
        out.extend_from_slice(d.title.as_bytes()); out.extend_from_slice(&d.payload);
        if out.len() > S_MAX_PAYLOAD as usize { return Err(VaultError::TooLarge); }
    }
    Ok(out)
}

pub fn decode_bundle(bytes: &[u8]) -> Result<(Vec<StoredDocument>, u64), VaultError> {
    if bytes.len() > S_MAX_PAYLOAD as usize || !bytes.starts_with(MAGIC) { return Err(VaultError::InvalidBundle("magic/size")); }
    let mut p = MAGIC.len();
    let next_id = take_u64(bytes, &mut p)?;
    let count = take_u32(bytes, &mut p)? as usize;
    if count > MAX_DOCS || next_id == 0 { return Err(VaultError::InvalidBundle("count/id")); }
    let mut docs = Vec::new(); let mut ids = std::collections::BTreeSet::new();
    for _ in 0..count {
        let id = take_u64(bytes, &mut p)?;
        let tl = take_u16(bytes, &mut p)? as usize;
        let pl = take_u32(bytes, &mut p)? as usize;
        let caret = take_u32(bytes, &mut p)?;
        if p.checked_add(tl).and_then(|n| n.checked_add(pl)).is_none_or(|n| n > bytes.len()) { return Err(VaultError::InvalidBundle("truncated entry")); }
        let title = std::str::from_utf8(&bytes[p..p+tl]).map_err(|_| VaultError::InvalidBundle("title encoding"))?.to_string(); p += tl;
        let payload = bytes[p..p+pl].to_vec(); p += pl;
        if id == 0 || id >= next_id || !ids.insert(id) || !valid_title(&title) { return Err(VaultError::InvalidBundle("entry id/title")); }
        let editor = Editor::from_payload(&payload).map_err(VaultError::Editor)?;
        if caret > editor.text_len() as u32 { return Err(VaultError::InvalidBundle("caret outside document")); }
        docs.push(StoredDocument { id, title, payload, caret });
    }
    if p != bytes.len() { return Err(VaultError::InvalidBundle("trailing bytes")); }
    Ok((docs, next_id))
}
fn take_u16(b: &[u8], p: &mut usize) -> Result<u16, VaultError> { let a = b.get(*p..*p+2).ok_or(VaultError::InvalidBundle("truncated integer"))?; *p += 2; Ok(u16::from_le_bytes(a.try_into().unwrap())) }
fn take_u32(b: &[u8], p: &mut usize) -> Result<u32, VaultError> { let a = b.get(*p..*p+4).ok_or(VaultError::InvalidBundle("truncated integer"))?; *p += 4; Ok(u32::from_le_bytes(a.try_into().unwrap())) }
fn take_u64(b: &[u8], p: &mut usize) -> Result<u64, VaultError> { let a = b.get(*p..*p+8).ok_or(VaultError::InvalidBundle("truncated integer"))?; *p += 8; Ok(u64::from_le_bytes(a.try_into().unwrap())) }
