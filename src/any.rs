//! Helper module which defines the `Any` trait to to allow dynamic value handling.

use crate::engine::{Array, Map};
use crate::intern::Str;
use crate::parser::INT;

#[cfg(not(feature = "no_float"))]
use crate::parser::FLOAT;

use crate::stdlib::{
    any::{type_name, Any, TypeId},
    boxed::Box,
    collections::HashMap,
    fmt,
    string::String,
    vec::Vec,
};

#[cfg(not(feature = "no_std"))]
use crate::stdlib::time::Instant;

/// A trait to represent any type.
///
/// Currently, `Variant` is not `Send` nor `Sync`, so it can practically be any type.
/// Turn on the `sync` feature to restrict it to only types that implement `Send + Sync`.
#[cfg(not(feature = "sync"))]
pub trait Variant: Any {
    /// Convert this `Variant` trait object to `&dyn Any`.
    fn as_any(&self) -> &dyn Any;

    /// Convert this `Variant` trait object to `&mut dyn Any`.
    fn as_mut_any(&mut self) -> &mut dyn Any;

    /// Get the name of this type.
    fn type_name(&self) -> &'static str;

    /// Convert into `Dynamic`.
    fn into_dynamic(self) -> Dynamic;

    /// Clone into `Dynamic`.
    fn clone_into_dynamic(&self) -> Dynamic;

    /// This trait may only be implemented by `rhai`.
    #[doc(hidden)]
    fn _closed(&self) -> _Private;
}

#[cfg(not(feature = "sync"))]
impl<T: Any + Clone> Variant for T {
    fn as_any(&self) -> &dyn Any {
        self as &dyn Any
    }
    fn as_mut_any(&mut self) -> &mut dyn Any {
        self as &mut dyn Any
    }
    fn type_name(&self) -> &'static str {
        type_name::<T>()
    }
    fn into_dynamic(self) -> Dynamic {
        Dynamic::from(self)
    }
    fn clone_into_dynamic(&self) -> Dynamic {
        Dynamic::from(self.clone())
    }
    fn _closed(&self) -> _Private {
        _Private
    }
}

/// A trait to represent any type.
///
/// `From<_>` is implemented for `i64` (`i32` if `only_i32`), `f64` (if not `no_float`),
/// `bool`, `String`, `char`, `Vec<T>` (into `Array`) and `HashMap<String, T>` (into `Map`).
#[cfg(feature = "sync")]
pub trait Variant: Any + Send + Sync {
    /// Convert this `Variant` trait object to `&dyn Any`.
    fn as_any(&self) -> &dyn Any;

    /// Convert this `Variant` trait object to `&mut dyn Any`.
    fn as_mut_any(&mut self) -> &mut dyn Any;

    /// Get the name of this type.
    fn type_name(&self) -> &'static str;

    /// Convert into `Dynamic`.
    fn into_dynamic(self) -> Dynamic;

    /// Clone into `Dynamic`.
    fn clone_into_dynamic(&self) -> Dynamic;

    /// This trait may only be implemented by `rhai`.
    #[doc(hidden)]
    fn _closed(&self) -> _Private;
}

#[cfg(feature = "sync")]
impl<T: Any + Clone + Send + Sync> Variant for T {
    fn as_any(&self) -> &dyn Any {
        self as &dyn Any
    }
    fn as_mut_any(&mut self) -> &mut dyn Any {
        self as &mut dyn Any
    }
    fn type_name(&self) -> &'static str {
        type_name::<T>()
    }
    fn into_dynamic(self) -> Dynamic {
        Dynamic::from(self)
    }
    fn clone_into_dynamic(&self) -> Dynamic {
        Dynamic::from(self.clone())
    }
    fn _closed(&self) -> _Private {
        _Private
    }
}

impl dyn Variant {
    /// Is this `Variant` a specific type?
    pub fn is<T: Any>(&self) -> bool {
        TypeId::of::<T>() == self.type_id()
    }

    /// Get a reference of a specific type to the `Variant`.
    /// Returns `None` if the cast fails.
    pub fn downcast_ref<T: Any>(&self) -> Option<&T> {
        Any::downcast_ref::<T>(self.as_any())
    }

    /// Get a mutable reference of a specific type to the `Variant`.
    /// Returns `None` if the cast fails.
    pub fn downcast_mut<T: Any>(&mut self) -> Option<&mut T> {
        Any::downcast_mut::<T>(self.as_mut_any())
    }
}

/// A dynamic type containing any value.
pub struct Dynamic(pub(crate) Union);

/// Internal `Dynamic` representation.
pub enum Union {
    Unit(()),
    Bool(bool),
    Str(Box<String>),
    Char(char),
    Int(INT),
    #[cfg(not(feature = "no_float"))]
    Float(FLOAT),
    Array(Box<Array>),
    Map(Box<Map>),
    Variant(Box<Box<dyn Variant>>),
}

impl Dynamic {
    /// Does this `Dynamic` hold a variant data type
    /// instead of one of the support system primitive types?
    pub fn is_variant(&self) -> bool {
        match self.0 {
            Union::Variant(_) => true,
            _ => false,
        }
    }

    /// Is the value held by this `Dynamic` a particular type?
    pub fn is<T: Variant + Clone>(&self) -> bool {
        self.type_id() == TypeId::of::<T>()
    }

    /// Get the TypeId of the value held by this `Dynamic`.
    pub fn type_id(&self) -> TypeId {
        match &self.0 {
            Union::Unit(_) => TypeId::of::<()>(),
            Union::Bool(_) => TypeId::of::<bool>(),
            Union::Str(_) => TypeId::of::<String>(),
            Union::Char(_) => TypeId::of::<char>(),
            Union::Int(_) => TypeId::of::<INT>(),
            #[cfg(not(feature = "no_float"))]
            Union::Float(_) => TypeId::of::<FLOAT>(),
            Union::Array(_) => TypeId::of::<Array>(),
            Union::Map(_) => TypeId::of::<Map>(),
            Union::Variant(value) => (***value).type_id(),
        }
    }

    /// Get the name of the type of the value held by this `Dynamic`.
    pub fn type_name(&self) -> &'static str {
        match &self.0 {
            Union::Unit(_) => "()",
            Union::Bool(_) => "bool",
            Union::Str(_) => "string",
            Union::Char(_) => "char",
            Union::Int(_) => type_name::<INT>(),
            #[cfg(not(feature = "no_float"))]
            Union::Float(_) => type_name::<FLOAT>(),
            Union::Array(_) => "array",
            Union::Map(_) => "map",

            #[cfg(not(feature = "no_std"))]
            Union::Variant(value) if value.is::<Instant>() => "timestamp",
            Union::Variant(value) => (***value).type_name(),
        }
    }
}

impl fmt::Display for Dynamic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Union::Unit(_) => write!(f, ""),
            Union::Bool(value) => write!(f, "{}", value),
            Union::Str(value) => write!(f, "{}", value),
            Union::Char(value) => write!(f, "{}", value),
            Union::Int(value) => write!(f, "{}", value),
            #[cfg(not(feature = "no_float"))]
            Union::Float(value) => write!(f, "{}", value),
            Union::Array(value) => write!(f, "{:?}", value),
            Union::Map(value) => write!(f, "#{:?}", value),
            Union::Variant(_) => write!(f, "?"),
        }
    }
}

impl fmt::Debug for Dynamic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Union::Unit(value) => write!(f, "{:?}", value),
            Union::Bool(value) => write!(f, "{:?}", value),
            Union::Str(value) => write!(f, "{:?}", value),
            Union::Char(value) => write!(f, "{:?}", value),
            Union::Int(value) => write!(f, "{:?}", value),
            #[cfg(not(feature = "no_float"))]
            Union::Float(value) => write!(f, "{:?}", value),
            Union::Array(value) => write!(f, "{:?}", value),
            Union::Map(value) => write!(f, "#{:?}", value),
            Union::Variant(_) => write!(f, "<dynamic>"),
        }
    }
}

impl Clone for Dynamic {
    fn clone(&self) -> Self {
        match &self.0 {
            Union::Unit(value) => Self(Union::Unit(value.clone())),
            Union::Bool(value) => Self(Union::Bool(value.clone())),
            Union::Str(value) => Self(Union::Str(value.clone())),
            Union::Char(value) => Self(Union::Char(value.clone())),
            Union::Int(value) => Self(Union::Int(value.clone())),
            #[cfg(not(feature = "no_float"))]
            Union::Float(value) => Self(Union::Float(value.clone())),
            Union::Array(value) => Self(Union::Array(value.clone())),
            Union::Map(value) => Self(Union::Map(value.clone())),
            Union::Variant(value) => (***value).clone_into_dynamic(),
        }
    }
}

impl Default for Dynamic {
    fn default() -> Self {
        Self(Union::Unit(()))
    }
}

/// Cast a Boxed type into another type.
fn cast_box<X: Variant, T: Variant>(item: Box<X>) -> Result<T, Box<X>> {
    // Only allow casting to the exact same type
    if TypeId::of::<X>() == TypeId::of::<T>() {
        // SAFETY: just checked whether we are pointing to the correct type
        unsafe {
            let raw: *mut dyn Any = Box::into_raw(item as Box<dyn Any>);
            Ok(*Box::from_raw(raw as *mut T))
        }
    } else {
        // Return the consumed item for chaining.
        Err(item)
    }
}

impl Dynamic {
    /// Create a `Dynamic` from any type.  A `Dynamic` value is simply returned as is.
    ///
    /// Beware that you need to pass in an `Array` type for it to be recognized as an `Array`.
    /// A `Vec<T>` does not get automatically converted to an `Array`, but will be a generic
    /// restricted trait object instead, because `Vec<T>` is not a supported standard type.
    ///
    /// Similarly, passing in a `HashMap<String, T>` will not get a `Map` but a trait object.
    ///
    /// # Examples
    ///
    /// ```
    /// use rhai::Dynamic;
    ///
    /// let result = Dynamic::from(42_i64);
    /// assert_eq!(result.type_name(), "i64");
    /// assert_eq!(result.to_string(), "42");
    ///
    /// let result = Dynamic::from("hello".to_string());
    /// assert_eq!(result.type_name(), "string");
    /// assert_eq!(result.to_string(), "hello");
    ///
    /// let new_result = Dynamic::from(result);
    /// assert_eq!(new_result.type_name(), "string");
    /// assert_eq!(new_result.to_string(), "hello");
    /// ```
    pub fn from<T: Variant + Clone>(value: T) -> Self {
        let dyn_value = &value as &dyn Variant;

        if let Some(result) = dyn_value.downcast_ref::<()>().cloned().map(Union::Unit) {
            return Self(result);
        } else if let Some(result) = dyn_value.downcast_ref::<bool>().cloned().map(Union::Bool) {
            return Self(result);
        } else if let Some(result) = dyn_value.downcast_ref::<INT>().cloned().map(Union::Int) {
            return Self(result);
        } else if let Some(result) = dyn_value.downcast_ref::<char>().cloned().map(Union::Char) {
            return Self(result);
        }

        #[cfg(not(feature = "no_float"))]
        {
            if let Some(result) = dyn_value.downcast_ref::<FLOAT>().cloned().map(Union::Float) {
                return Self(result);
            }
        }

        let var = Box::new(value);

        Self(
            cast_box::<_, Dynamic>(var)
                .map(|x| x.0)
                .or_else(|var| {
                    cast_box::<_, String>(var)
                        .map(Box::new)
                        .map(Union::Str)
                        .or_else(|var| {
                            cast_box::<_, Array>(var)
                                .map(Box::new)
                                .map(Union::Array)
                                .or_else(|var| {
                                    cast_box::<_, Map>(var)
                                        .map(Box::new)
                                        .map(Union::Map)
                                        .or_else(|var| -> Result<Union, ()> {
                                            Ok(Union::Variant(Box::new(var as Box<dyn Variant>)))
                                        })
                                })
                        })
                })
                .unwrap(),
        )
    }

    /// Get a copy of the `Dynamic` value as a specific type.
    /// Casting to a `Dynamic` just returns as is.
    ///
    /// Returns an error with the name of the value's actual type when the cast fails.
    ///
    /// # Example
    ///
    /// ```
    /// use rhai::Dynamic;
    ///
    /// let x = Dynamic::from(42_u32);
    ///
    /// assert_eq!(x.try_cast::<u32>().unwrap(), 42);
    /// ```
    pub fn try_cast<T: Variant + Clone>(self) -> Option<T> {
        if TypeId::of::<T>() == TypeId::of::<Dynamic>() {
            return cast_box::<_, T>(Box::new(self)).ok();
        }

        match self.0 {
            Union::Unit(ref value) => (value as &dyn Variant).downcast_ref::<T>().cloned(),
            Union::Bool(ref value) => (value as &dyn Variant).downcast_ref::<T>().cloned(),
            Union::Str(value) => cast_box::<_, T>(value).ok(),
            Union::Char(ref value) => (value as &dyn Variant).downcast_ref::<T>().cloned(),
            Union::Int(ref value) => (value as &dyn Variant).downcast_ref::<T>().cloned(),
            #[cfg(not(feature = "no_float"))]
            Union::Float(ref value) => (value as &dyn Variant).downcast_ref::<T>().cloned(),
            Union::Array(value) => cast_box::<_, T>(value).ok(),
            Union::Map(value) => cast_box::<_, T>(value).ok(),
            Union::Variant(value) => value.as_ref().as_ref().downcast_ref::<T>().cloned(),
        }
    }

    /// Get a copy of the `Dynamic` value as a specific type.
    /// Casting to a `Dynamic` just returns as is.
    ///
    /// # Panics
    ///
    /// Panics if the cast fails (e.g. the type of the actual value is not the same as the specified type).
    ///
    /// # Example
    ///
    /// ```
    /// use rhai::Dynamic;
    ///
    /// let x = Dynamic::from(42_u32);
    ///
    /// assert_eq!(x.cast::<u32>(), 42);
    /// ```
    pub fn cast<T: Variant + Clone>(self) -> T {
        self.try_cast::<T>().unwrap()
    }

    /// Get a reference of a specific type to the `Dynamic`.
    /// Casting to `Dynamic` just returns a reference to it.
    /// Returns `None` if the cast fails.
    pub fn downcast_ref<T: Variant + Clone>(&self) -> Option<&T> {
        if TypeId::of::<T>() == TypeId::of::<Dynamic>() {
            return (self as &dyn Variant).downcast_ref::<T>();
        }

        match &self.0 {
            Union::Unit(value) => (value as &dyn Variant).downcast_ref::<T>(),
            Union::Bool(value) => (value as &dyn Variant).downcast_ref::<T>(),
            Union::Str(value) => (value.as_ref() as &dyn Variant).downcast_ref::<T>(),
            Union::Char(value) => (value as &dyn Variant).downcast_ref::<T>(),
            Union::Int(value) => (value as &dyn Variant).downcast_ref::<T>(),
            #[cfg(not(feature = "no_float"))]
            Union::Float(value) => (value as &dyn Variant).downcast_ref::<T>(),
            Union::Array(value) => (value.as_ref() as &dyn Variant).downcast_ref::<T>(),
            Union::Map(value) => (value.as_ref() as &dyn Variant).downcast_ref::<T>(),
            Union::Variant(value) => value.as_ref().as_ref().downcast_ref::<T>(),
        }
    }

    /// Get a mutable reference of a specific type to the `Dynamic`.
    /// Casting to `Dynamic` just returns a mutable reference to it.
    /// Returns `None` if the cast fails.
    pub fn downcast_mut<T: Variant + Clone>(&mut self) -> Option<&mut T> {
        if TypeId::of::<T>() == TypeId::of::<Dynamic>() {
            return (self as &mut dyn Variant).downcast_mut::<T>();
        }

        match &mut self.0 {
            Union::Unit(value) => (value as &mut dyn Variant).downcast_mut::<T>(),
            Union::Bool(value) => (value as &mut dyn Variant).downcast_mut::<T>(),
            Union::Str(value) => (value.as_mut() as &mut dyn Variant).downcast_mut::<T>(),
            Union::Char(value) => (value as &mut dyn Variant).downcast_mut::<T>(),
            Union::Int(value) => (value as &mut dyn Variant).downcast_mut::<T>(),
            #[cfg(not(feature = "no_float"))]
            Union::Float(value) => (value as &mut dyn Variant).downcast_mut::<T>(),
            Union::Array(value) => (value.as_mut() as &mut dyn Variant).downcast_mut::<T>(),
            Union::Map(value) => (value.as_mut() as &mut dyn Variant).downcast_mut::<T>(),
            Union::Variant(value) => value.as_mut().as_mut().downcast_mut::<T>(),
        }
    }

    /// Cast the `Dynamic` as the system integer type `INT` and return it.
    /// Returns the name of the actual type if the cast fails.
    pub fn as_int(&self) -> Result<INT, &'static str> {
        match self.0 {
            Union::Int(n) => Ok(n),
            _ => Err(self.type_name()),
        }
    }

    /// Cast the `Dynamic` as a `bool` and return it.
    /// Returns the name of the actual type if the cast fails.
    pub fn as_bool(&self) -> Result<bool, &'static str> {
        match self.0 {
            Union::Bool(b) => Ok(b),
            _ => Err(self.type_name()),
        }
    }

    /// Cast the `Dynamic` as a `char` and return it.
    /// Returns the name of the actual type if the cast fails.
    pub fn as_char(&self) -> Result<char, &'static str> {
        match self.0 {
            Union::Char(n) => Ok(n),
            _ => Err(self.type_name()),
        }
    }

    /// Cast the `Dynamic` as a string and return the string slice.
    /// Returns the name of the actual type if the cast fails.
    pub fn as_str(&self) -> Result<&str, &'static str> {
        match &self.0 {
            Union::Str(s) => Ok(s),
            _ => Err(self.type_name()),
        }
    }

    /// Convert the `Dynamic` into `String` and return it.
    /// Returns the name of the actual type if the cast fails.
    pub fn take_string(self) -> Result<String, &'static str> {
        match self.0 {
            Union::Str(s) => Ok(*s),
            _ => Err(self.type_name()),
        }
    }
}

impl From<()> for Dynamic {
    fn from(value: ()) -> Self {
        Self(Union::Unit(value))
    }
}
impl From<bool> for Dynamic {
    fn from(value: bool) -> Self {
        Self(Union::Bool(value))
    }
}
impl From<INT> for Dynamic {
    fn from(value: INT) -> Self {
        Self(Union::Int(value))
    }
}
#[cfg(not(feature = "no_float"))]
impl From<FLOAT> for Dynamic {
    fn from(value: FLOAT) -> Self {
        Self(Union::Float(value))
    }
}
impl From<char> for Dynamic {
    fn from(value: char) -> Self {
        Self(Union::Char(value))
    }
}
impl From<String> for Dynamic {
    fn from(value: String) -> Self {
        Self(Union::Str(Box::new(value)))
    }
}
impl From<&Str> for Dynamic {
    fn from(value: &Str) -> Self {
        Self(Union::Str(Box::new(value.to_string())))
    }
}

impl<T: Variant + Clone> From<Vec<T>> for Dynamic {
    fn from(value: Vec<T>) -> Self {
        Self(Union::Array(Box::new(
            value.into_iter().map(Dynamic::from).collect(),
        )))
    }
}
impl<T: Variant + Clone> From<HashMap<String, T>> for Dynamic {
    fn from(value: HashMap<String, T>) -> Self {
        Self(Union::Map(Box::new(
            value
                .into_iter()
                .map(|(k, v)| (k, Dynamic::from(v)))
                .collect(),
        )))
    }
}

/// Private type which ensures that `rhai::Any` and `rhai::AnyExt` can only
/// be implemented by this crate.
#[doc(hidden)]
pub struct _Private;
