#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]

#[cfg(feature = "serde")]
mod serde;
#[cfg(test)]
mod test_formats;

include!("./generated.rs");

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::CStr;
use core::fmt;
use core::mem::MaybeUninit;
use core::ops::{Deref, DerefMut};
use core::ptr;

#[cfg(feature = "std")]
use alloc::ffi::CString;
#[cfg(feature = "std")]
use std::path::Path;

const BYTES_PER_G1_POINT: usize = 48;
const BYTES_PER_G2_POINT: usize = 96;

/// Number of G1 points required for the kzg trusted setup.
const NUM_G1_POINTS: usize = 4096;

/// Number of G2 points required for the kzg trusted setup.
/// 65 is fixed and is used for providing multiproofs up to 64 field elements.
const NUM_G2_POINTS: usize = 65;

/// A trusted (valid) KZG commitment.
// NOTE: this is a type alias to the struct Bytes48, same as [`KZGProof`] in the C header files. To
//       facilitate type safety: proofs and commitments should not be interchangeable, we use a
//       custom implementation.
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct KZGCommitment {
    bytes: [u8; BYTES_PER_COMMITMENT],
}

/// A trusted (valid) KZG proof.
// NOTE: this is a type alias to the struct Bytes48, same as [`KZGCommitment`] in the C header
//       files. To facilitate type safety: proofs and commitments should not be interchangeable, we
//       use a custom implementation.
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct KZGProof {
    bytes: [u8; BYTES_PER_PROOF],
}

#[derive(Debug)]
pub enum Error {
    /// Wrong number of bytes.
    InvalidBytesLength(String),
    /// The hex string is invalid.
    InvalidHexFormat(String),
    /// The KZG proof is invalid.
    InvalidKzgProof(String),
    /// The KZG commitment is invalid.
    InvalidKzgCommitment(String),
    /// The provided trusted setup is invalid.
    InvalidTrustedSetup(String),
    /// Paired arguments have different lengths.
    MismatchLength(String),
    /// Loading the trusted setup failed.
    LoadingTrustedSetupFailed(KzgErrors),
    /// The underlying c-kzg library returned an error.
    CError(C_KZG_RET),
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBytesLength(s)
            | Self::InvalidHexFormat(s)
            | Self::InvalidKzgProof(s)
            | Self::InvalidKzgCommitment(s)
            | Self::InvalidTrustedSetup(s)
            | Self::MismatchLength(s) => f.write_str(s),
            Self::LoadingTrustedSetupFailed(s) => write!(f, "KzgErrors: {s:?}"),
            Self::CError(s) => fmt::Debug::fmt(s, f),
        }
    }
}

impl From<KzgErrors> for Error {
    fn from(e: KzgErrors) -> Self {
        Error::LoadingTrustedSetupFailed(e)
    }
}

pub type CellsPerExtBlob = [Cell; CELLS_PER_EXT_BLOB];
pub type ProofsPerExtBlob = [KZGProof; CELLS_PER_EXT_BLOB];

#[derive(Debug)]
pub enum KzgErrors {
    /// Failed to get current directory.
    FailedCurrentDirectory,
    /// The specified path does not exist.
    PathNotExists,
    /// Problems related to I/O.
    IOError,
    /// Not a valid file.
    NotValidFile,
    /// File is not properly formatted.
    FileFormatError,
    /// Not able to parse to usize.
    ParseError,
    /// Number of points does not match what is expected.
    MismatchedNumberOfPoints,
}

/// Converts a hex string (with or without the 0x prefix) to bytes.
pub fn hex_to_bytes(hex_str: &str) -> Result<Vec<u8>, Error> {
    let trimmed_str = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    hex::decode(trimmed_str)
        .map_err(|e| Error::InvalidHexFormat(format!("Failed to decode hex: {e}")))
}

/// Holds the parameters of a kzg trusted setup ceremony.
impl KZGSettings {
    /// Initializes a trusted setup from a flat array of `FIELD_ELEMENTS_PER_BLOB` G1 points in monomial form, a flat
    /// array of `FIELD_ELEMENTS_PER_BLOB` G1 points in Lagrange form, and a flat array of 65 G2 points in monomial
    /// form.
    pub fn load_trusted_setup(
        g1_monomial_bytes: &[u8],
        g1_lagrange_bytes: &[u8],
        g2_monomial_bytes: &[u8],
        precompute: u64,
    ) -> Result<Self, Error> {
        let mut kzg_settings = MaybeUninit::<KZGSettings>::uninit();
        unsafe {
            let res = load_trusted_setup(
                kzg_settings.as_mut_ptr(),
                g1_monomial_bytes.as_ptr().cast(),
                g1_monomial_bytes.len() as u64,
                g1_lagrange_bytes.as_ptr().cast(),
                g1_lagrange_bytes.len() as u64,
                g2_monomial_bytes.as_ptr().cast(),
                g2_monomial_bytes.len() as u64,
                precompute,
            );
            if let C_KZG_RET::C_KZG_OK = res {
                Ok(kzg_settings.assume_init())
            } else {
                Err(Error::InvalidTrustedSetup(format!(
                    "Invalid trusted setup: {res:?}",
                )))
            }
        }
    }

    /// Loads the trusted setup parameters from a file. The file format is as follows:
    ///
    /// FIELD_ELEMENTS_PER_BLOB
    /// 65 # This is fixed and is used for providing multiproofs up to 64 field elements.
    /// FIELD_ELEMENT_PER_BLOB g1 byte values in Lagrange form
    /// 65 g2 byte values in monomial form
    /// FIELD_ELEMENT_PER_BLOB g1 byte values in monomial form
    #[cfg(feature = "std")]
    pub fn load_trusted_setup_file(file_path: &Path, precompute: u64) -> Result<Self, Error> {
        #[cfg(unix)]
        let file_path_bytes = {
            use std::os::unix::prelude::OsStrExt;
            file_path.as_os_str().as_bytes()
        };

        #[cfg(windows)]
        let file_path_bytes = file_path
            .as_os_str()
            .to_str()
            .ok_or_else(|| Error::InvalidTrustedSetup("Unsupported non unicode file path".into()))?
            .as_bytes();

        let file_path = CString::new(file_path_bytes)
            .map_err(|e| Error::InvalidTrustedSetup(format!("Invalid trusted setup file: {e}")))?;

        Self::load_trusted_setup_file_inner(&file_path, precompute)
    }

    /// Parses the contents of a KZG trusted setup file into a KzgSettings.
    pub fn parse_kzg_trusted_setup(trusted_setup: &str, precompute: u64) -> Result<Self, Error> {
        let mut lines = trusted_setup.lines();

        // Load number of g1 points
        let n_g1 = lines
            .next()
            .ok_or(KzgErrors::FileFormatError)?
            .parse::<usize>()
            .map_err(|_| KzgErrors::ParseError)?;
        if n_g1 != NUM_G1_POINTS {
            return Err(KzgErrors::MismatchedNumberOfPoints.into());
        }

        // Load number of g2 points
        let n_g2 = lines
            .next()
            .ok_or(KzgErrors::FileFormatError)?
            .parse::<usize>()
            .map_err(|_| KzgErrors::ParseError)?;
        if n_g2 != NUM_G2_POINTS {
            return Err(KzgErrors::MismatchedNumberOfPoints.into());
        }

        let mut g1_lagrange_bytes = alloc::boxed::Box::new([0; BYTES_PER_G1_POINT * NUM_G1_POINTS]);
        let mut g2_monomial_bytes = alloc::boxed::Box::new([0; BYTES_PER_G2_POINT * NUM_G2_POINTS]);
        let mut g1_monomial_bytes = alloc::boxed::Box::new([0; BYTES_PER_G1_POINT * NUM_G1_POINTS]);

        // Load g1 Lagrange bytes
        g1_lagrange_bytes
            .chunks_mut(BYTES_PER_G1_POINT)
            .try_for_each(|chunk| {
                let line = lines.next().ok_or(KzgErrors::FileFormatError)?;
                hex::decode_to_slice(line, chunk).map_err(|_| KzgErrors::ParseError)
            })?;

        // Load g2 monomial bytes
        g2_monomial_bytes
            .chunks_mut(BYTES_PER_G2_POINT)
            .try_for_each(|chunk| {
                let line = lines.next().ok_or(KzgErrors::FileFormatError)?;
                hex::decode_to_slice(line, chunk).map_err(|_| KzgErrors::ParseError)
            })?;

        // Load g1 monomial bytes
        g1_monomial_bytes
            .chunks_mut(BYTES_PER_G1_POINT)
            .try_for_each(|chunk| {
                let line = lines.next().ok_or(KzgErrors::FileFormatError)?;
                hex::decode_to_slice(line, chunk).map_err(|_| KzgErrors::ParseError)
            })?;

        if lines.next().is_some() {
            return Err(KzgErrors::FileFormatError.into());
        }

        Self::load_trusted_setup(
            g1_monomial_bytes.as_ref(),
            g1_lagrange_bytes.as_ref(),
            g2_monomial_bytes.as_ref(),
            precompute,
        )
    }

    /// Loads the trusted setup parameters from a file. The file format is as follows:
    ///
    /// FIELD_ELEMENTS_PER_BLOB
    /// 65 # This is fixed and is used for providing multiproofs up to 64 field elements.
    /// FIELD_ELEMENT_PER_BLOB g1 byte values in Lagrange form
    /// 65 g2 byte values in monomial form
    /// FIELD_ELEMENT_PER_BLOB g1 byte values in monomial form
    #[cfg(not(feature = "std"))]
    pub fn load_trusted_setup_file(file_path: &CStr, precompute: u64) -> Result<Self, Error> {
        Self::load_trusted_setup_file_inner(file_path, precompute)
    }

    /// Loads the trusted setup parameters from a file.
    ///
    /// Same as [`load_trusted_setup_file`](Self::load_trusted_setup_file)
    #[cfg_attr(not(feature = "std"), doc = ", but takes a `CStr` instead of a `Path`")]
    pub fn load_trusted_setup_file_inner(file_path: &CStr, precompute: u64) -> Result<Self, Error> {
        // SAFETY: `b"r\0"` is a valid null-terminated string.
        const MODE: &CStr = c"r";

        // SAFETY:
        // - .as_ptr(): pointer is not dangling because file_path has not been dropped.
        //    Usage or ptr: File will not be written to it by the c code.
        let file_ptr = unsafe { libc::fopen(file_path.as_ptr(), MODE.as_ptr()) };
        if file_ptr.is_null() {
            #[cfg(not(feature = "std"))]
            return Err(Error::InvalidTrustedSetup(format!(
                "Failed to open trusted setup file {file_path:?}"
            )));

            #[cfg(feature = "std")]
            return Err(Error::InvalidTrustedSetup(format!(
                "Failed to open trusted setup file {file_path:?}: {}",
                std::io::Error::last_os_error()
            )));
        }
        let mut kzg_settings = MaybeUninit::<KZGSettings>::uninit();

        unsafe {
            let res = load_trusted_setup_file(kzg_settings.as_mut_ptr(), file_ptr, precompute);
            let _unchecked_close_result = libc::fclose(file_ptr);

            if let C_KZG_RET::C_KZG_OK = res {
                Ok(kzg_settings.assume_init())
            } else {
                Err(Error::InvalidTrustedSetup(format!(
                    "Invalid trusted setup: {res:?}"
                )))
            }
        }
    }

    pub fn blob_to_kzg_commitment(&self, blob: &Blob) -> Result<KZGCommitment, Error> {
        let mut kzg_commitment: MaybeUninit<KZGCommitment> = MaybeUninit::uninit();
        unsafe {
            let res = blob_to_kzg_commitment(kzg_commitment.as_mut_ptr(), blob, self);
            if let C_KZG_RET::C_KZG_OK = res {
                Ok(kzg_commitment.assume_init())
            } else {
                Err(Error::CError(res))
            }
        }
    }

    pub fn compute_kzg_proof(
        &self,
        blob: &Blob,
        z_bytes: &Bytes32,
    ) -> Result<(KZGProof, Bytes32), Error> {
        let mut kzg_proof = MaybeUninit::<KZGProof>::uninit();
        let mut y_out = MaybeUninit::<Bytes32>::uninit();
        unsafe {
            let res = compute_kzg_proof(
                kzg_proof.as_mut_ptr(),
                y_out.as_mut_ptr(),
                blob,
                z_bytes,
                self,
            );
            if let C_KZG_RET::C_KZG_OK = res {
                Ok((kzg_proof.assume_init(), y_out.assume_init()))
            } else {
                Err(Error::CError(res))
            }
        }
    }

    pub fn compute_blob_kzg_proof(
        &self,
        blob: &Blob,
        commitment_bytes: &Bytes48,
    ) -> Result<KZGProof, Error> {
        let mut kzg_proof = MaybeUninit::<KZGProof>::uninit();
        unsafe {
            let res = compute_blob_kzg_proof(kzg_proof.as_mut_ptr(), blob, commitment_bytes, self);
            if let C_KZG_RET::C_KZG_OK = res {
                Ok(kzg_proof.assume_init())
            } else {
                Err(Error::CError(res))
            }
        }
    }

    pub fn verify_kzg_proof(
        &self,
        commitment_bytes: &Bytes48,
        z_bytes: &Bytes32,
        y_bytes: &Bytes32,
        proof_bytes: &Bytes48,
    ) -> Result<bool, Error> {
        let mut verified: MaybeUninit<bool> = MaybeUninit::uninit();
        unsafe {
            let res = verify_kzg_proof(
                verified.as_mut_ptr(),
                commitment_bytes,
                z_bytes,
                y_bytes,
                proof_bytes,
                self,
            );
            if let C_KZG_RET::C_KZG_OK = res {
                Ok(verified.assume_init())
            } else {
                Err(Error::CError(res))
            }
        }
    }

    pub fn verify_blob_kzg_proof(
        &self,
        blob: &Blob,
        commitment_bytes: &Bytes48,
        proof_bytes: &Bytes48,
    ) -> Result<bool, Error> {
        let mut verified: MaybeUninit<bool> = MaybeUninit::uninit();
        unsafe {
            let res = verify_blob_kzg_proof(
                verified.as_mut_ptr(),
                blob,
                commitment_bytes,
                proof_bytes,
                self,
            );
            if let C_KZG_RET::C_KZG_OK = res {
                Ok(verified.assume_init())
            } else {
                Err(Error::CError(res))
            }
        }
    }

    pub fn verify_blob_kzg_proof_batch(
        &self,
        blobs: &[Blob],
        commitments_bytes: &[Bytes48],
        proofs_bytes: &[Bytes48],
    ) -> Result<bool, Error> {
        if blobs.len() != commitments_bytes.len() {
            return Err(Error::MismatchLength(format!(
                "There are {} blobs and {} commitments",
                blobs.len(),
                commitments_bytes.len()
            )));
        }
        if blobs.len() != proofs_bytes.len() {
            return Err(Error::MismatchLength(format!(
                "There are {} blobs and {} proofs",
                blobs.len(),
                proofs_bytes.len()
            )));
        }
        let mut verified: MaybeUninit<bool> = MaybeUninit::uninit();
        unsafe {
            let res = verify_blob_kzg_proof_batch(
                verified.as_mut_ptr(),
                blobs.as_ptr(),
                commitments_bytes.as_ptr(),
                proofs_bytes.as_ptr(),
                blobs.len() as u64,
                self,
            );
            if let C_KZG_RET::C_KZG_OK = res {
                Ok(verified.assume_init())
            } else {
                Err(Error::CError(res))
            }
        }
    }

    pub fn compute_cells(&self, blob: &Blob) -> Result<Box<CellsPerExtBlob>, Error> {
        let mut cells = [Cell::default(); CELLS_PER_EXT_BLOB];
        unsafe {
            let res = compute_cells_and_kzg_proofs(cells.as_mut_ptr(), ptr::null_mut(), blob, self);
            if let C_KZG_RET::C_KZG_OK = res {
                Ok(Box::new(cells))
            } else {
                Err(Error::CError(res))
            }
        }
    }

    pub fn compute_cells_and_kzg_proofs(
        &self,
        blob: &Blob,
    ) -> Result<(Box<CellsPerExtBlob>, Box<ProofsPerExtBlob>), Error> {
        let mut cells = [Cell::default(); CELLS_PER_EXT_BLOB];
        let mut proofs = [KZGProof::default(); CELLS_PER_EXT_BLOB];
        unsafe {
            let res =
                compute_cells_and_kzg_proofs(cells.as_mut_ptr(), proofs.as_mut_ptr(), blob, self);
            if let C_KZG_RET::C_KZG_OK = res {
                Ok((Box::new(cells), Box::new(proofs)))
            } else {
                Err(Error::CError(res))
            }
        }
    }

    pub fn recover_cells_and_kzg_proofs(
        &self,
        cell_indices: &[u64],
        cells: &[Cell],
    ) -> Result<(Box<CellsPerExtBlob>, Box<ProofsPerExtBlob>), Error> {
        if cell_indices.len() != cells.len() {
            return Err(Error::MismatchLength(format!(
                "There are {} cell indices and {} cells",
                cell_indices.len(),
                cells.len()
            )));
        }
        let mut recovered_cells = [Cell::default(); CELLS_PER_EXT_BLOB];
        let mut recovered_proofs = [KZGProof::default(); CELLS_PER_EXT_BLOB];
        unsafe {
            let res = recover_cells_and_kzg_proofs(
                recovered_cells.as_mut_ptr(),
                recovered_proofs.as_mut_ptr(),
                cell_indices.as_ptr(),
                cells.as_ptr(),
                cells.len() as u64,
                self,
            );
            if let C_KZG_RET::C_KZG_OK = res {
                Ok((Box::new(recovered_cells), Box::new(recovered_proofs)))
            } else {
                Err(Error::CError(res))
            }
        }
    }

    pub fn verify_cell_kzg_proof_batch(
        &self,
        commitments_bytes: &[Bytes48],
        cell_indices: &[u64],
        cells: &[Cell],
        proofs_bytes: &[Bytes48],
    ) -> Result<bool, Error> {
        if cells.len() != commitments_bytes.len() {
            return Err(Error::MismatchLength(format!(
                "There are {} cells and {} commitments",
                cells.len(),
                commitments_bytes.len()
            )));
        }
        if cells.len() != cell_indices.len() {
            return Err(Error::MismatchLength(format!(
                "There are {} cells and {} column indices",
                cells.len(),
                cell_indices.len()
            )));
        }
        if cells.len() != proofs_bytes.len() {
            return Err(Error::MismatchLength(format!(
                "There are {} cells and {} proofs",
                cells.len(),
                proofs_bytes.len()
            )));
        }
        let mut verified: MaybeUninit<bool> = MaybeUninit::uninit();
        unsafe {
            let res = verify_cell_kzg_proof_batch(
                verified.as_mut_ptr(),
                commitments_bytes.as_ptr(),
                cell_indices.as_ptr(),
                cells.as_ptr(),
                proofs_bytes.as_ptr(),
                cells.len() as u64,
                self,
            );
            if let C_KZG_RET::C_KZG_OK = res {
                Ok(verified.assume_init())
            } else {
                Err(Error::CError(res))
            }
        }
    }
}

impl Drop for KZGSettings {
    fn drop(&mut self) {
        unsafe { free_trusted_setup(self) }
    }
}

impl Blob {
    /// Creates a new blob from a byte array.
    pub const fn new(bytes: [u8; BYTES_PER_BLOB]) -> Self {
        Self { bytes }
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != BYTES_PER_BLOB {
            return Err(Error::InvalidBytesLength(format!(
                "Invalid byte length. Expected {} got {}",
                BYTES_PER_BLOB,
                bytes.len(),
            )));
        }
        let mut new_bytes = [0; BYTES_PER_BLOB];
        new_bytes.copy_from_slice(bytes);
        Ok(Self::new(new_bytes))
    }

    pub fn from_hex(hex_str: &str) -> Result<Self, Error> {
        Self::from_bytes(&hex_to_bytes(hex_str)?)
    }

    pub fn into_inner(self) -> [u8; BYTES_PER_BLOB] {
        self.bytes
    }
}

impl AsRef<[u8]> for Blob {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl Bytes32 {
    /// Creates a new instance from a byte array.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self { bytes }
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != 32 {
            return Err(Error::InvalidBytesLength(format!(
                "Invalid byte length. Expected {} got {}",
                32,
                bytes.len(),
            )));
        }
        let mut new_bytes = [0; 32];
        new_bytes.copy_from_slice(bytes);
        Ok(Self::new(new_bytes))
    }

    pub fn from_hex(hex_str: &str) -> Result<Self, Error> {
        Self::from_bytes(&hex_to_bytes(hex_str)?)
    }
}

impl Bytes48 {
    /// Creates a new instance from a byte array.
    pub const fn new(bytes: [u8; 48]) -> Self {
        Self { bytes }
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != 48 {
            return Err(Error::InvalidBytesLength(format!(
                "Invalid byte length. Expected {} got {}",
                48,
                bytes.len(),
            )));
        }
        let mut new_bytes = [0; 48];
        new_bytes.copy_from_slice(bytes);
        Ok(Self::new(new_bytes))
    }

    pub fn from_hex(hex_str: &str) -> Result<Self, Error> {
        Self::from_bytes(&hex_to_bytes(hex_str)?)
    }

    pub fn into_inner(self) -> [u8; 48] {
        self.bytes
    }
}

impl KZGProof {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != BYTES_PER_PROOF {
            return Err(Error::InvalidKzgProof(format!(
                "Invalid byte length. Expected {} got {}",
                BYTES_PER_PROOF,
                bytes.len(),
            )));
        }
        let mut proof_bytes = [0; BYTES_PER_PROOF];
        proof_bytes.copy_from_slice(bytes);
        Ok(Self { bytes: proof_bytes })
    }

    pub fn to_bytes(&self) -> Bytes48 {
        Bytes48 { bytes: self.bytes }
    }

    pub fn as_hex_string(&self) -> String {
        hex::encode(self.bytes)
    }
}

impl KZGCommitment {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != BYTES_PER_COMMITMENT {
            return Err(Error::InvalidKzgCommitment(format!(
                "Invalid byte length. Expected {} got {}",
                BYTES_PER_PROOF,
                bytes.len(),
            )));
        }
        let mut commitment = [0; BYTES_PER_COMMITMENT];
        commitment.copy_from_slice(bytes);
        Ok(Self { bytes: commitment })
    }

    pub fn to_bytes(&self) -> Bytes48 {
        Bytes48 { bytes: self.bytes }
    }

    pub fn as_hex_string(&self) -> String {
        hex::encode(self.bytes)
    }
}

impl Cell {
    pub const fn new(bytes: [u8; BYTES_PER_CELL]) -> Self {
        Self { bytes }
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != BYTES_PER_CELL {
            return Err(Error::InvalidBytesLength(format!(
                "Invalid byte length. Expected {} got {}",
                BYTES_PER_CELL,
                bytes.len(),
            )));
        }
        let mut new_bytes = [0; BYTES_PER_CELL];
        new_bytes.copy_from_slice(bytes);
        Ok(Self::new(new_bytes))
    }

    pub fn to_bytes(&self) -> [u8; BYTES_PER_CELL] {
        self.bytes
    }

    pub fn from_hex(hex_str: &str) -> Result<Self, Error> {
        Self::from_bytes(&hex_to_bytes(hex_str)?)
    }
}

impl From<[u8; BYTES_PER_COMMITMENT]> for KZGCommitment {
    fn from(value: [u8; BYTES_PER_COMMITMENT]) -> Self {
        Self { bytes: value }
    }
}

impl From<[u8; BYTES_PER_PROOF]> for KZGProof {
    fn from(value: [u8; BYTES_PER_PROOF]) -> Self {
        Self { bytes: value }
    }
}

impl From<[u8; BYTES_PER_BLOB]> for Blob {
    fn from(value: [u8; BYTES_PER_BLOB]) -> Self {
        Self { bytes: value }
    }
}

impl From<[u8; 32]> for Bytes32 {
    fn from(value: [u8; 32]) -> Self {
        Self { bytes: value }
    }
}

impl AsRef<[u8; 32]> for Bytes32 {
    fn as_ref(&self) -> &[u8; 32] {
        &self.bytes
    }
}

impl From<[u8; 48]> for Bytes48 {
    fn from(value: [u8; 48]) -> Self {
        Self { bytes: value }
    }
}

impl AsRef<[u8; 48]> for Bytes48 {
    fn as_ref(&self) -> &[u8; 48] {
        &self.bytes
    }
}

impl Deref for Bytes32 {
    type Target = [u8; 32];
    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}

impl Deref for Bytes48 {
    type Target = [u8; 48];
    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}

impl DerefMut for Bytes48 {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.bytes
    }
}

impl Deref for Blob {
    type Target = [u8; BYTES_PER_BLOB];
    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}

impl DerefMut for Blob {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.bytes
    }
}

impl Clone for Blob {
    fn clone(&self) -> Self {
        Blob { bytes: self.bytes }
    }
}

impl Deref for KZGProof {
    type Target = [u8; BYTES_PER_PROOF];
    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}

impl Deref for KZGCommitment {
    type Target = [u8; BYTES_PER_COMMITMENT];
    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}

#[allow(clippy::derivable_impls)]
impl Default for Bytes32 {
    fn default() -> Self {
        Bytes32 { bytes: [0; 32] }
    }
}

impl Default for Bytes48 {
    fn default() -> Self {
        Bytes48 { bytes: [0; 48] }
    }
}

impl Default for KZGCommitment {
    fn default() -> Self {
        KZGCommitment {
            bytes: [0; BYTES_PER_COMMITMENT],
        }
    }
}

impl Default for KZGProof {
    fn default() -> Self {
        KZGProof {
            bytes: [0; BYTES_PER_PROOF],
        }
    }
}

impl Default for Blob {
    fn default() -> Self {
        Blob {
            bytes: [0; BYTES_PER_BLOB],
        }
    }
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            bytes: [0; BYTES_PER_CELL],
        }
    }
}

#[cfg(feature = "arbitrary")]
impl arbitrary::Arbitrary<'_> for Bytes32 {
    fn arbitrary(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<Self> {
        let mut bytes = [0u8; 32];
        u.fill_buffer(&mut bytes)?;
        Ok(Bytes32::from(bytes))
    }
}

#[cfg(feature = "arbitrary")]
impl arbitrary::Arbitrary<'_> for Bytes48 {
    fn arbitrary(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<Self> {
        let mut bytes = [0u8; 48];
        u.fill_buffer(&mut bytes)?;
        Ok(Bytes48::from(bytes))
    }
}

#[cfg(feature = "arbitrary")]
impl arbitrary::Arbitrary<'_> for Blob {
    fn arbitrary(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<Self> {
        let mut bytes = [0u8; BYTES_PER_BLOB];
        u.fill_buffer(&mut bytes)?;
        Ok(Blob::from(bytes))
    }
}

#[cfg(feature = "arbitrary")]
impl arbitrary::Arbitrary<'_> for Cell {
    fn arbitrary(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<Self> {
        let mut bytes = [0u8; BYTES_PER_CELL];
        u.fill_buffer(&mut bytes)?;
        Ok(Cell::new(bytes))
    }
}

/// Safety: The memory for `roots_of_unity` and `g1_values` and `g2_values` are only freed on
/// calling `free_trusted_setup` which only happens when we drop the struct.
unsafe impl Sync for KZGSettings {}
unsafe impl Send for KZGSettings {}

#[cfg(test)]
#[allow(unused_imports, dead_code)]
mod tests {
    use super::*;
    use rand::{rngs::ThreadRng, Rng};
    use std::{fs, path::PathBuf};
    use test_formats::{
        blob_to_kzg_commitment_test, compute_blob_kzg_proof, compute_cells,
        compute_cells_and_kzg_proofs, compute_kzg_proof, recover_cells_and_kzg_proofs,
        verify_blob_kzg_proof, verify_blob_kzg_proof_batch, verify_cell_kzg_proof_batch,
        verify_kzg_proof,
    };

    fn generate_random_blob(rng: &mut ThreadRng) -> Blob {
        let mut arr = [0u8; BYTES_PER_BLOB];
        rng.fill(&mut arr[..]);
        // Ensure that the blob is canonical by ensuring that
        // each field element contained in the blob is < BLS_MODULUS
        for i in 0..FIELD_ELEMENTS_PER_BLOB {
            arr[i * BYTES_PER_FIELD_ELEMENT] = 0;
        }
        arr.into()
    }

    fn test_simple(trusted_setup_file: &Path) {
        let mut rng = rand::rng();
        assert!(trusted_setup_file.exists());
        let kzg_settings = KZGSettings::load_trusted_setup_file(trusted_setup_file, 0).unwrap();

        let num_blobs: usize = rng.random_range(1..16);
        let mut blobs: Vec<Blob> = (0..num_blobs)
            .map(|_| generate_random_blob(&mut rng))
            .collect();

        let commitments: Vec<Bytes48> = blobs
            .iter()
            .map(|blob| kzg_settings.blob_to_kzg_commitment(blob).unwrap())
            .map(|commitment| commitment.to_bytes())
            .collect();

        let proofs: Vec<Bytes48> = blobs
            .iter()
            .zip(commitments.iter())
            .map(|(blob, commitment)| {
                kzg_settings
                    .compute_blob_kzg_proof(blob, commitment)
                    .unwrap()
            })
            .map(|proof| proof.to_bytes())
            .collect();

        assert!(kzg_settings
            .verify_blob_kzg_proof_batch(&blobs, &commitments, &proofs)
            .unwrap());

        blobs.pop();

        let error = kzg_settings
            .verify_blob_kzg_proof_batch(&blobs, &commitments, &proofs)
            .unwrap_err();
        assert!(matches!(error, Error::MismatchLength(_)));

        let incorrect_blob = generate_random_blob(&mut rng);
        blobs.push(incorrect_blob);

        assert!(!kzg_settings
            .verify_blob_kzg_proof_batch(&blobs, &commitments, &proofs)
            .unwrap());
    }

    #[test]
    fn test_end_to_end() {
        let trusted_setup_file = Path::new("src/trusted_setup.txt");
        test_simple(trusted_setup_file);
    }

    const BLOB_TO_KZG_COMMITMENT_TESTS: &str = "tests/blob_to_kzg_commitment/*/*/*";
    const COMPUTE_KZG_PROOF_TESTS: &str = "tests/compute_kzg_proof/*/*/*";
    const COMPUTE_BLOB_KZG_PROOF_TESTS: &str = "tests/compute_blob_kzg_proof/*/*/*";
    const VERIFY_KZG_PROOF_TESTS: &str = "tests/verify_kzg_proof/*/*/*";
    const VERIFY_BLOB_KZG_PROOF_TESTS: &str = "tests/verify_blob_kzg_proof/*/*/*";
    const VERIFY_BLOB_KZG_PROOF_BATCH_TESTS: &str = "tests/verify_blob_kzg_proof_batch/*/*/*";

    const COMPUTE_CELLS_TESTS: &str = "tests/compute_cells/*/*/*";
    const COMPUTE_CELLS_AND_KZG_PROOFS_TESTS: &str = "tests/compute_cells_and_kzg_proofs/*/*/*";
    const RECOVER_CELLS_AND_KZG_PROOFS_TESTS: &str = "tests/recover_cells_and_kzg_proofs/*/*/*";
    const VERIFY_CELL_KZG_PROOF_BATCH_TESTS: &str = "tests/verify_cell_kzg_proof_batch/*/*/*";

    #[test]
    fn test_blob_to_kzg_commitment() {
        let trusted_setup_file = Path::new("src/trusted_setup.txt");
        assert!(trusted_setup_file.exists());
        let kzg_settings = KZGSettings::load_trusted_setup_file(trusted_setup_file, 0).unwrap();
        let test_files: Vec<PathBuf> = glob::glob(BLOB_TO_KZG_COMMITMENT_TESTS)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(!test_files.is_empty());

        #[allow(unused_variables)]
        for (index, test_file) in test_files.iter().enumerate() {
            let yaml_data = fs::read_to_string(test_file).unwrap();
            let test: blob_to_kzg_commitment_test::Test = serde_yaml::from_str(&yaml_data).unwrap();
            let Ok(blob) = test.input.get_blob() else {
                assert!(test.get_output().is_none());
                continue;
            };

            #[cfg(feature = "generate-fuzz-corpus")]
            {
                use std::{env, fs::File, io::Write};
                let root_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
                let dir_path = root_dir
                    .join("fuzz")
                    .join("corpus")
                    .join("fuzz_blob_to_kzg_commitment");
                fs::create_dir_all(&dir_path).unwrap();
                let file_path = dir_path.join(format!("data_{index}.bin"));
                let mut file = File::create(&file_path).unwrap();
                file.write_all(&blob.bytes).unwrap();
            }

            match kzg_settings.blob_to_kzg_commitment(&blob) {
                Ok(res) => assert_eq!(res.bytes, test.get_output().unwrap().bytes),
                _ => assert!(test.get_output().is_none()),
            }
        }
    }

    #[test]
    fn test_parse_kzg_trusted_setup() {
        let trusted_setup_file = Path::new("src/trusted_setup.txt");
        assert!(trusted_setup_file.exists());
        let trusted_setup = fs::read_to_string(trusted_setup_file).unwrap();
        let _ = KZGSettings::parse_kzg_trusted_setup(&trusted_setup, 0).unwrap();
    }

    #[test]
    fn test_compute_kzg_proof() {
        let trusted_setup_file = Path::new("src/trusted_setup.txt");
        assert!(trusted_setup_file.exists());
        let kzg_settings = KZGSettings::load_trusted_setup_file(trusted_setup_file, 0).unwrap();
        let test_files: Vec<PathBuf> = glob::glob(COMPUTE_KZG_PROOF_TESTS)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(!test_files.is_empty());

        #[allow(unused_variables)]
        for (index, test_file) in test_files.iter().enumerate() {
            let yaml_data = fs::read_to_string(test_file).unwrap();
            let test: compute_kzg_proof::Test = serde_yaml::from_str(&yaml_data).unwrap();
            let (Ok(blob), Ok(z)) = (test.input.get_blob(), test.input.get_z()) else {
                assert!(test.get_output().is_none());
                continue;
            };

            #[cfg(feature = "generate-fuzz-corpus")]
            {
                use std::{env, fs::File, io::Write};
                let root_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
                let dir_path = root_dir
                    .join("fuzz")
                    .join("corpus")
                    .join("fuzz_compute_kzg_proof");
                fs::create_dir_all(&dir_path).unwrap();
                let file_path = dir_path.join(format!("data_{index}.bin"));
                let mut file = File::create(&file_path).unwrap();
                file.write_all(&blob.bytes).unwrap();
                file.write_all(&z.bytes).unwrap();
            }

            match kzg_settings.compute_kzg_proof(&blob, &z) {
                Ok((proof, y)) => {
                    assert_eq!(proof.bytes, test.get_output().unwrap().0.bytes);
                    assert_eq!(y.bytes, test.get_output().unwrap().1.bytes);
                }
                _ => assert!(test.get_output().is_none()),
            }
        }
    }

    #[test]
    fn test_compute_blob_kzg_proof() {
        let trusted_setup_file = Path::new("src/trusted_setup.txt");
        assert!(trusted_setup_file.exists());
        let kzg_settings = KZGSettings::load_trusted_setup_file(trusted_setup_file, 0).unwrap();
        let test_files: Vec<PathBuf> = glob::glob(COMPUTE_BLOB_KZG_PROOF_TESTS)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(!test_files.is_empty());

        #[allow(unused_variables)]
        for (index, test_file) in test_files.iter().enumerate() {
            let yaml_data = fs::read_to_string(test_file).unwrap();
            let test: compute_blob_kzg_proof::Test = serde_yaml::from_str(&yaml_data).unwrap();
            let (Ok(blob), Ok(commitment)) = (test.input.get_blob(), test.input.get_commitment())
            else {
                assert!(test.get_output().is_none());
                continue;
            };

            #[cfg(feature = "generate-fuzz-corpus")]
            {
                use std::{env, fs::File, io::Write};
                let root_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
                let dir_path = root_dir
                    .join("fuzz")
                    .join("corpus")
                    .join("fuzz_compute_blob_kzg_proof");
                fs::create_dir_all(&dir_path).unwrap();
                let file_path = dir_path.join(format!("data_{index}.bin"));
                let mut file = File::create(&file_path).unwrap();
                file.write_all(&blob.bytes).unwrap();
                file.write_all(&commitment.bytes).unwrap();
            }

            match kzg_settings.compute_blob_kzg_proof(&blob, &commitment) {
                Ok(res) => assert_eq!(res.bytes, test.get_output().unwrap().bytes),
                _ => assert!(test.get_output().is_none()),
            }
        }
    }

    #[test]
    fn test_verify_kzg_proof() {
        let trusted_setup_file = Path::new("src/trusted_setup.txt");
        assert!(trusted_setup_file.exists());
        let kzg_settings = KZGSettings::load_trusted_setup_file(trusted_setup_file, 0).unwrap();
        let test_files: Vec<PathBuf> = glob::glob(VERIFY_KZG_PROOF_TESTS)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(!test_files.is_empty());

        #[allow(unused_variables)]
        for (index, test_file) in test_files.iter().enumerate() {
            let yaml_data = fs::read_to_string(test_file).unwrap();
            let test: verify_kzg_proof::Test = serde_yaml::from_str(&yaml_data).unwrap();
            let (Ok(commitment), Ok(z), Ok(y), Ok(proof)) = (
                test.input.get_commitment(),
                test.input.get_z(),
                test.input.get_y(),
                test.input.get_proof(),
            ) else {
                assert!(test.get_output().is_none());
                continue;
            };

            #[cfg(feature = "generate-fuzz-corpus")]
            {
                use std::{env, fs::File, io::Write};
                let root_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
                let dir_path = root_dir
                    .join("fuzz")
                    .join("corpus")
                    .join("fuzz_verify_kzg_proof");
                fs::create_dir_all(&dir_path).unwrap();
                let file_path = dir_path.join(format!("data_{index}.bin"));
                let mut file = File::create(&file_path).unwrap();
                file.write_all(&commitment.bytes).unwrap();
                file.write_all(&z.bytes).unwrap();
                file.write_all(&y.bytes).unwrap();
                file.write_all(&proof.bytes).unwrap();
            }

            match kzg_settings.verify_kzg_proof(&commitment, &z, &y, &proof) {
                Ok(res) => assert_eq!(res, test.get_output().unwrap()),
                _ => assert!(test.get_output().is_none()),
            }
        }
    }

    #[test]
    fn test_verify_blob_kzg_proof() {
        let trusted_setup_file = Path::new("src/trusted_setup.txt");
        assert!(trusted_setup_file.exists());
        let kzg_settings = KZGSettings::load_trusted_setup_file(trusted_setup_file, 0).unwrap();
        let test_files: Vec<PathBuf> = glob::glob(VERIFY_BLOB_KZG_PROOF_TESTS)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(!test_files.is_empty());

        #[allow(unused_variables)]
        for (index, test_file) in test_files.iter().enumerate() {
            let yaml_data = fs::read_to_string(test_file).unwrap();
            let test: verify_blob_kzg_proof::Test = serde_yaml::from_str(&yaml_data).unwrap();
            let (Ok(blob), Ok(commitment), Ok(proof)) = (
                test.input.get_blob(),
                test.input.get_commitment(),
                test.input.get_proof(),
            ) else {
                assert!(test.get_output().is_none());
                continue;
            };

            #[cfg(feature = "generate-fuzz-corpus")]
            {
                use std::{env, fs::File, io::Write};
                let root_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
                let dir_path = root_dir
                    .join("fuzz")
                    .join("corpus")
                    .join("fuzz_verify_blob_kzg_proof");
                fs::create_dir_all(&dir_path).unwrap();
                let file_path = dir_path.join(format!("data_{index}.bin"));
                let mut file = File::create(&file_path).unwrap();
                file.write_all(&blob.bytes).unwrap();
                file.write_all(&commitment.bytes).unwrap();
                file.write_all(&proof.bytes).unwrap();
            }

            match kzg_settings.verify_blob_kzg_proof(&blob, &commitment, &proof) {
                Ok(res) => assert_eq!(res, test.get_output().unwrap()),
                _ => assert!(test.get_output().is_none()),
            }
        }
    }

    #[test]
    fn test_verify_blob_kzg_proof_batch() {
        let trusted_setup_file = Path::new("src/trusted_setup.txt");
        assert!(trusted_setup_file.exists());
        let kzg_settings = KZGSettings::load_trusted_setup_file(trusted_setup_file, 0).unwrap();
        let test_files: Vec<PathBuf> = glob::glob(VERIFY_BLOB_KZG_PROOF_BATCH_TESTS)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(!test_files.is_empty());

        #[allow(unused_variables)]
        for (index, test_file) in test_files.iter().enumerate() {
            let yaml_data = fs::read_to_string(test_file).unwrap();
            let test: verify_blob_kzg_proof_batch::Test = serde_yaml::from_str(&yaml_data).unwrap();
            let (Ok(blobs), Ok(commitments), Ok(proofs)) = (
                test.input.get_blobs(),
                test.input.get_commitments(),
                test.input.get_proofs(),
            ) else {
                assert!(test.get_output().is_none());
                continue;
            };

            #[cfg(feature = "generate-fuzz-corpus")]
            {
                use std::{env, fs::File, io::Write};
                let root_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
                let dir_path = root_dir
                    .join("fuzz")
                    .join("corpus")
                    .join("fuzz_verify_blob_kzg_proof_batch");
                fs::create_dir_all(&dir_path).unwrap();
                let file_path = dir_path.join(format!("data_{index}.bin"));
                let mut file = File::create(&file_path).unwrap();
                for blob in &blobs {
                    file.write_all(&blob.bytes).unwrap();
                }
                for commitment in &commitments {
                    file.write_all(&commitment.bytes).unwrap();
                }
                for proof in &proofs {
                    file.write_all(&proof.bytes).unwrap();
                }
            }

            match kzg_settings.verify_blob_kzg_proof_batch(&blobs, &commitments, &proofs) {
                Ok(res) => assert_eq!(res, test.get_output().unwrap()),
                _ => assert!(test.get_output().is_none()),
            }
        }
    }

    #[test]
    fn test_compute_cells() {
        let trusted_setup_file = Path::new("src/trusted_setup.txt");
        assert!(trusted_setup_file.exists());
        let kzg_settings = KZGSettings::load_trusted_setup_file(trusted_setup_file, 0).unwrap();
        let test_files: Vec<PathBuf> = glob::glob(COMPUTE_CELLS_TESTS)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(!test_files.is_empty());

        #[allow(unused_variables)]
        for (index, test_file) in test_files.iter().enumerate() {
            let yaml_data = fs::read_to_string(test_file).unwrap();
            let test: compute_cells::Test = serde_yaml::from_str(&yaml_data).unwrap();
            let Ok(blob) = test.input.get_blob() else {
                assert!(test.get_output().is_none());
                continue;
            };

            #[cfg(feature = "generate-fuzz-corpus")]
            {
                use std::{env, fs::File, io::Write};
                let root_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
                let dir_path = root_dir
                    .join("fuzz")
                    .join("corpus")
                    .join("fuzz_compute_cells");
                fs::create_dir_all(&dir_path).unwrap();
                let file_path = dir_path.join(format!("data_{index}.bin"));
                let mut file = File::create(&file_path).unwrap();
                file.write_all(&blob.bytes).unwrap();
            }

            match kzg_settings.compute_cells(&blob) {
                Ok(res) => assert_eq!(res.as_slice(), test.get_output().unwrap()),
                _ => assert!(test.get_output().is_none()),
            }
        }
    }

    #[test]
    fn test_compute_cells_and_kzg_proofs() {
        let trusted_setup_file = Path::new("src/trusted_setup.txt");
        assert!(trusted_setup_file.exists());
        let kzg_settings = KZGSettings::load_trusted_setup_file(trusted_setup_file, 0).unwrap();
        let test_files: Vec<PathBuf> = glob::glob(COMPUTE_CELLS_AND_KZG_PROOFS_TESTS)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(!test_files.is_empty());

        #[allow(unused_variables)]
        for (index, test_file) in test_files.iter().enumerate() {
            let yaml_data = fs::read_to_string(test_file).unwrap();
            let test: compute_cells_and_kzg_proofs::Test =
                serde_yaml::from_str(&yaml_data).unwrap();
            let Ok(blob) = test.input.get_blob() else {
                assert!(test.get_output().is_none());
                continue;
            };

            #[cfg(feature = "generate-fuzz-corpus")]
            {
                use std::{env, fs::File, io::Write};
                let root_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
                let dir_path = root_dir
                    .join("fuzz")
                    .join("corpus")
                    .join("fuzz_compute_cells_and_kzg_proofs");
                fs::create_dir_all(&dir_path).unwrap();
                let file_path = dir_path.join(format!("data_{index}.bin"));
                let mut file = File::create(&file_path).unwrap();
                file.write_all(&blob.bytes).unwrap();
            }

            match kzg_settings.compute_cells_and_kzg_proofs(&blob) {
                Ok((cells, proofs)) => {
                    let (expected_cells, expected_proofs) = test.get_output().unwrap();
                    assert_eq!(cells.as_slice(), expected_cells);
                    let proofs_as_bytes: Vec<Bytes48> =
                        proofs.iter().map(|p| p.to_bytes()).collect();
                    assert_eq!(proofs_as_bytes, expected_proofs);
                }
                _ => assert!(test.get_output().is_none()),
            }
        }
    }

    #[test]
    fn test_recover_cells_and_kzg_proofs() {
        let trusted_setup_file = Path::new("src/trusted_setup.txt");
        assert!(trusted_setup_file.exists());
        let kzg_settings = KZGSettings::load_trusted_setup_file(trusted_setup_file, 0).unwrap();
        let test_files: Vec<PathBuf> = glob::glob(RECOVER_CELLS_AND_KZG_PROOFS_TESTS)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(!test_files.is_empty());

        #[allow(unused_variables)]
        for (index, test_file) in test_files.iter().enumerate() {
            let yaml_data = fs::read_to_string(test_file).unwrap();
            let test: recover_cells_and_kzg_proofs::Test =
                serde_yaml::from_str(&yaml_data).unwrap();
            let (Ok(cell_indices), Ok(cells)) =
                (test.input.get_cell_indices(), test.input.get_cells())
            else {
                assert!(test.get_output().is_none());
                continue;
            };

            #[cfg(feature = "generate-fuzz-corpus")]
            {
                use std::{env, fs::File, io::Write};
                let root_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
                let dir_path = root_dir
                    .join("fuzz")
                    .join("corpus")
                    .join("fuzz_recover_cells_and_kzg_proofs");
                fs::create_dir_all(&dir_path).unwrap();
                let file_path = dir_path.join(format!("data_{index}.bin"));
                let mut file = File::create(&file_path).unwrap();
                for cell_index in &cell_indices {
                    file.write_all(&cell_index.to_le_bytes()).unwrap();
                }
                for cell in &cells {
                    file.write_all(&cell.bytes).unwrap();
                }
            }

            match kzg_settings.recover_cells_and_kzg_proofs(&cell_indices, &cells) {
                Ok((recovered_cells, recovered_proofs)) => {
                    let (expected_cells, expected_proofs) = test.get_output().unwrap();
                    assert_eq!(recovered_cells.as_slice(), expected_cells);
                    let proofs_as_bytes: Vec<Bytes48> =
                        recovered_proofs.iter().map(|p| p.to_bytes()).collect();
                    assert_eq!(proofs_as_bytes, expected_proofs);
                }
                _ => assert!(test.get_output().is_none()),
            }
        }
    }

    #[test]
    fn test_verify_cell_kzg_proof_batch() {
        let trusted_setup_file = Path::new("src/trusted_setup.txt");
        assert!(trusted_setup_file.exists());
        let kzg_settings = KZGSettings::load_trusted_setup_file(trusted_setup_file, 0).unwrap();
        let test_files: Vec<PathBuf> = glob::glob(VERIFY_CELL_KZG_PROOF_BATCH_TESTS)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(!test_files.is_empty());

        #[allow(unused_variables)]
        for (index, test_file) in test_files.iter().enumerate() {
            let yaml_data = fs::read_to_string(test_file).unwrap();
            let test: verify_cell_kzg_proof_batch::Test = serde_yaml::from_str(&yaml_data).unwrap();
            let (Ok(commitments), Ok(cell_indices), Ok(cells), Ok(proofs)) = (
                test.input.get_commitments(),
                test.input.get_cell_indices(),
                test.input.get_cells(),
                test.input.get_proofs(),
            ) else {
                assert!(test.get_output().is_none());
                continue;
            };

            #[cfg(feature = "generate-fuzz-corpus")]
            {
                use std::{env, fs::File, io::Write};
                let root_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
                let dir_path = root_dir
                    .join("fuzz")
                    .join("corpus")
                    .join("fuzz_verify_cell_kzg_proof_batch");
                fs::create_dir_all(&dir_path).unwrap();
                let file_path = dir_path.join(format!("data_{index}.bin"));
                let mut file = File::create(&file_path).unwrap();
                for commitment in &commitments {
                    file.write_all(&commitment.bytes).unwrap();
                }
                for cell_index in &cell_indices {
                    file.write_all(&cell_index.to_le_bytes()).unwrap();
                }
                for cell in &cells {
                    file.write_all(&cell.bytes).unwrap();
                }
                for proof in &proofs {
                    file.write_all(&proof.bytes).unwrap();
                }
            }

            match kzg_settings.verify_cell_kzg_proof_batch(
                &commitments,
                &cell_indices,
                &cells,
                &proofs,
            ) {
                Ok(res) => assert_eq!(res, test.get_output().unwrap()),
                _ => assert!(test.get_output().is_none()),
            }
        }
    }
}
