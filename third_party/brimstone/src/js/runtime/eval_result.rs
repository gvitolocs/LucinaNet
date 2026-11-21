#[cfg(feature = "alloc_error")]
use crate::runtime::alloc_error::AllocError;
use crate::runtime::{Context, Handle, gc::Escapable, value::Value};

/// Any JS value can be thrown as an evaluation error.
///
/// Wrap in a zero-cost newtype for type shenanigans.
#[derive(Clone, Copy)]
pub enum EvalError {
    /// A thrown error JS value.
    Value(Handle<Value>),
    /// An allocation error.
    #[cfg(feature = "alloc_error")]
    Alloc(AllocError),
    /// Uncatchable termination requested by an embedder host (a native that fails without an
    /// exception, or a refused interrupt). Unwinds every JS frame without running `catch` or
    /// `finally` blocks, up to the outermost call from the host.
    Uncatchable,
}

impl EvalError {
    #[inline]
    pub fn new_value(value: Handle<Value>) -> Self {
        EvalError::Value(value)
    }

    #[cfg(feature = "alloc_error")]
    #[inline]
    pub fn new_alloc(err: AllocError) -> Self {
        EvalError::Alloc(err)
    }

    #[cfg(feature = "alloc_error")]
    #[inline]
    pub fn into_alloc(self) -> AllocError {
        match self {
            EvalError::Alloc(err) => err,
            _ => panic!("Expected EvalError::Alloc"),
        }
    }
}

impl std::fmt::Debug for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Value(value) => write!(f, "EvalError::Value({:#x})", value.as_raw_bits()),
            #[cfg(feature = "alloc_error")]
            Self::Alloc(_) => write!(f, "EvalError::Alloc"),
            Self::Uncatchable => write!(f, "EvalError::Uncatchable"),
        }
    }
}

impl Escapable for EvalError {
    #[inline]
    fn escape(&self, cx: Context) -> Self {
        match self {
            Self::Value(value) => Self::Value(value.escape(cx)),
            #[cfg(feature = "alloc_error")]
            Self::Alloc(err) => Self::Alloc(err.escape(cx)),
            Self::Uncatchable => Self::Uncatchable,
        }
    }
}

/// EvalResult is for functions which are either successful or throw a value.
pub type EvalResult<T> = Result<T, EvalError>;

/// Unwrap an EvalResult that must never throw
#[macro_export]
macro_rules! must {
    ($a:expr) => {{
        use $crate::runtime::{
            eval_result::{EvalError, EvalResult},
            intrinsics::error_object::ErrorObject,
        };

        let result = $a;
        match result {
            EvalResult::Ok(value) => value,
            // Propagate OOMs upwards instead of failing the assertion
            #[cfg(feature = "alloc_error")]
            Err(EvalError::Alloc(alloc_err)) => {
                return Err(alloc_err.into());
            }
            // A thrown value. Propagate upwards only if it is a stack overflow, otherwise fail
            // the assertion.
            Err(EvalError::Value(value)) => {
                if let Some(error) = value.as_opt::<ErrorObject>() {
                    if error.is_stack_overflow() {
                        return Err(EvalError::Value(value.into()));
                    }
                }

                panic!("Unexpected abnormal completion")
            }
            // Uncatchable termination always propagates
            Err(EvalError::Uncatchable) => return Err(EvalError::Uncatchable),
        }
    }};
}

/// Unwrap an EvalResult that must never throw inside an AllocResult
#[macro_export]
macro_rules! must_a {
    ($a:expr) => {{
        use $crate::runtime::eval_result::{EvalError, EvalResult};

        let result = $a;
        match result {
            EvalResult::Ok(value) => value,
            // Propagate OOMs upwards instead of failing the assertion
            #[cfg(feature = "alloc_error")]
            Err(EvalError::Alloc(alloc_err)) => {
                return Err(alloc_err.into());
            }
            // Fail assertion on any thrown value including stack overflows
            Err(EvalError::Value(_)) => {
                panic!("Unexpected abnormal completion")
            }
            Err(EvalError::Uncatchable) => {
                panic!("Unexpected uncatchable termination")
            }
        }
    }};
}

/// Create a new EvalError from a JS value
#[macro_export]
macro_rules! eval_err {
    ($value:expr) => {
        Err($crate::runtime::eval_result::EvalError::new_value($value))
    };
}

#[macro_export]
macro_rules! completion_value {
    ($result:expr) => {
        match $result {
            Ok(value) => Ok(value),
            Err($crate::runtime::eval_result::EvalError::Value(value)) => Err(value),
            #[cfg(feature = "alloc_error")]
            Err($crate::runtime::eval_result::EvalError::Alloc(err)) => return Err(err.into()),
            // Uncatchable termination is never turned into a completion value
            Err($crate::runtime::eval_result::EvalError::Uncatchable) => {
                return Err($crate::runtime::eval_result::EvalError::Uncatchable)
            }
        }
    };
    // For callers that cannot propagate an uncatchable termination (they return an AllocResult).
    // The termination is reported as a throw completion of undefined.
    ($result:expr, $cx:expr) => {
        match $result {
            Ok(value) => Ok(value),
            Err($crate::runtime::eval_result::EvalError::Value(value)) => Err(value),
            #[cfg(feature = "alloc_error")]
            Err($crate::runtime::eval_result::EvalError::Alloc(err)) => return Err(err.into()),
            Err($crate::runtime::eval_result::EvalError::Uncatchable) => Err($cx.undefined()),
        }
    };
}
