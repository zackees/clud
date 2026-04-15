use crate::{WhisperError, WhisperSegment, WhisperTokenData, WhisperTokenId};
use std::borrow::Cow;
use std::ffi::{c_int, CStr};
use std::fmt;

pub struct WhisperToken<'a, 'b: 'a> {
    segment: &'a WhisperSegment<'b>,
    token_idx: c_int,
}

impl<'a, 'b> WhisperToken<'a, 'b> {
    /// # Safety
    /// You must ensure `token_idx` is in bounds for this [`WhisperSegment`].
    pub(crate) unsafe fn new_unchecked(segment: &'a WhisperSegment<'b>, token_idx: c_int) -> Self {
        Self { segment, token_idx }
    }

    /// Get the token ID of this token in its segment.
    ///
    /// # Returns
    /// [`WhisperTokenId`]
    ///
    /// # C++ equivalent
    /// `whisper_token whisper_full_get_token_id(struct whisper_context * ctx, int i_segment, int i_token)`
    pub fn token_id(&self) -> WhisperTokenId {
        unsafe {
            whisper_rs_sys::whisper_full_get_token_id_from_state(
                self.segment.get_state().ptr,
                self.segment.segment_index(),
                self.token_idx,
            )
        }
    }

    /// Get token data for this token in its segment.
    ///
    /// # Returns
    /// [`WhisperTokenData`]
    ///
    /// # C++ equivalent
    /// `whisper_token_data whisper_full_get_token_data(struct whisper_context * ctx, int i_segment, int i_token)`
    pub fn token_data(&self) -> WhisperTokenData {
        unsafe {
            whisper_rs_sys::whisper_full_get_token_data_from_state(
                self.segment.get_state().ptr,
                self.segment.segment_index(),
                self.token_idx,
            )
        }
    }

    /// Get the probability of this token in its segment.
    ///
    /// # Returns
    /// `f32`
    ///
    /// # C++ equivalent
    /// `float whisper_full_get_token_p(struct whisper_context * ctx, int i_segment, int i_token)`
    pub fn token_probability(&self) -> f32 {
        unsafe {
            whisper_rs_sys::whisper_full_get_token_p_from_state(
                self.segment.get_state().ptr,
                self.segment.segment_index(),
                self.token_idx,
            )
        }
    }

    fn to_raw_cstr(&self) -> Result<&'b CStr, WhisperError> {
        let ret = unsafe {
            whisper_rs_sys::whisper_full_get_token_text_from_state(
                self.segment.get_state().ctx.ctx,
                self.segment.get_state().ptr,
                self.segment.segment_index(),
                self.token_idx,
            )
        };
        if ret.is_null() {
            return Err(WhisperError::NullPointer);
        }
        Ok(unsafe { CStr::from_ptr(ret) })
    }

    /// Get the raw bytes of this token.
    ///
    /// Useful if you're using a language for which Whisper is known to split tokens
    /// away from UTF-8 character boundaries.
    ///
    /// # Returns
    /// * On success: The raw bytes, with no null terminator
    /// * On failure: [`WhisperError::NullPointer`]
    ///
    /// # C++ equivalent
    /// `const char * whisper_full_get_token_text(struct whisper_context * ctx, int i_segment, int i_token)`
    pub fn to_bytes(&self) -> Result<&'b [u8], WhisperError> {
        Ok(self.to_raw_cstr()?.to_bytes())
    }

    /// Get the text of this token.
    ///
    /// # Returns
    /// * On success: the UTF-8 validated string.
    /// * On failure: [`WhisperError::NullPointer`] or [`WhisperError::InvalidUtf8`]
    ///
    /// # C++ equivalent
    /// `const char * whisper_full_get_token_text(struct whisper_context * ctx, int i_segment, int i_token)`
    pub fn to_str(&self) -> Result<&'b str, WhisperError> {
        Ok(self.to_raw_cstr()?.to_str()?)
    }

    /// Get the text of this token.
    ///
    /// This function differs from [`Self::to_str`]
    /// in that it ignores invalid UTF-8 in strings,
    /// and instead replaces it with the replacement character.
    ///
    /// # Returns
    /// * On success: The valid string, with any invalid UTF-8 replaced with the replacement character
    /// * On failure: [`WhisperError::NullPointer`]
    ///
    /// # C++ equivalent
    /// `const char * whisper_full_get_token_text(struct whisper_context * ctx, int i_segment, int i_token)`
    pub fn to_str_lossy(&self) -> Result<Cow<'b, str>, WhisperError> {
        Ok(self.to_raw_cstr()?.to_string_lossy())
    }
}

/// Write the contents of this token to the output.
/// This will panic if Whisper returns a null pointer.
///
/// Uses [`Self::to_str_lossy`] internally.
impl fmt::Display for WhisperToken<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            self.to_str_lossy()
                .expect("got null pointer during string write")
        )
    }
}

impl fmt::Debug for WhisperToken<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WhisperToken")
            .field("segment_idx", &self.segment.segment_index())
            .field("token_idx", &self.token_idx)
            .field("token_id", &self.token_id())
            .field("token_data", &self.token_data())
            .field("token_probability", &self.token_probability())
            .finish()
    }
}
