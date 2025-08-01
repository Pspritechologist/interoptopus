pub mod builtins;
pub mod class;
pub mod constants;
pub mod docs;
pub mod functions;
pub mod imports;
pub mod namespace;
pub mod patterns;
pub mod types;

use crate::converter::param_to_type;
use crate::interop::builtins::write_builtins;
use crate::interop::class::{write_class_context, write_native_lib_string};
use crate::interop::constants::write_constants;
use crate::interop::docs::write_file_header_comments;
use crate::interop::functions::write_functions;
use crate::interop::imports::write_imports;
use crate::interop::namespace::write_namespace_context;
use crate::interop::patterns::abi_guard::write_abi_guard;
use crate::interop::patterns::asynk::write_pattern_async_trampoline_initializers;
use crate::interop::patterns::write_patterns;
use crate::interop::types::write_type_definitions;
use derive_builder::Builder;
use interoptopus::backend::IndentWriter;
use interoptopus::backend::{NamespaceMappings, is_global_type};
use interoptopus::inventory::{Bindings, Inventory};
use interoptopus::lang::{Constant, Function, Meta, Signature, Type};
use interoptopus::pattern::TypePattern;
use interoptopus::{Error, indented};

const FILE_HEADER: &str = r"// <auto-generated>
//
// This file was automatically generated by Interoptopus.
//
// Library:      {INTEROP_DLL_NAME}
// Hash:         0x{INTEROP_HASH}
// Namespace:    {INTEROP_NAMESPACE}
// Builder:      {INTEROPTOPUS_CRATE}
//
// Do not edit this file manually.
//
// </auto-generated>
";

/// How to convert from Rust function names to C#
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FunctionNameFlavor<'a> {
    /// Takes the name as it is written in Rust
    RawFFIName,
    /// Converts the name to camel case
    CSharpMethodWithClass,
    /// Converts the name to camel case and removes the class name
    CSharpMethodWithoutClass(&'a str),
}

/// The types to write for the given recorder.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WriteTypes {
    /// Only write items defined in the library for this namespace.
    Namespace,
    /// Write types in this namespace and global interoptopus types (e.g., `FFIBool`)
    NamespaceAndInteroptopusGlobal,
    /// Write every type in the library, regardless of namespace association.
    All,
}

impl WriteTypes {
    #[must_use]
    pub const fn write_interoptopus_globals(self) -> bool {
        match self {
            Self::Namespace => false,
            Self::NamespaceAndInteroptopusGlobal => true,
            Self::All => true,
        }
    }
}

/// The access modifiers for generated `CSharp` types
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Visibility {
    /// Mimics Rust visibility.
    AsDeclared,
    /// Generates all types as `public class` / `public struct`.
    ForcePublic,
    /// Generates all types as `internal class` / `internal struct`.
    ForceInternal,
}

impl Visibility {
    #[must_use]
    pub const fn to_access_modifier(self) -> &'static str {
        match self {
            // TODO: `AsDeclared` should ultimately use the declared visibility but for now copy the previous
            //        behavior which is to make everything public.
            Self::AsDeclared => "public",
            Self::ForcePublic => "public",
            Self::ForceInternal => "internal",
        }
    }
}

impl Default for Interop {
    fn default() -> Self {
        Self {
            inventory: Inventory::default(),
            file_header_comment: FILE_HEADER.to_string(),
            class: "Interop".to_string(),
            class_constants: None,
            dll_name: "library".to_string(),
            namespace_mappings: NamespaceMappings::new("My.Company"),
            namespace_id: String::new(),
            visibility_types: Visibility::AsDeclared,
            write_types: WriteTypes::NamespaceAndInteroptopusGlobal,
            debug: false,
            doc_hints: true,
        }
    }
}

/// Generates C# interop files, **get this with [`InteropBuilder`]**.🐙
#[derive(Clone, Debug, Builder)]
#[builder(default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Interop {
    /// The file header, e.g., `// (c) My Company`.
    #[builder(setter(into))]
    file_header_comment: String,
    /// Name of static class for Interop methods, e.g., `Interop`.
    #[builder(setter(into))]
    class: String,
    /// Name of static class for Interop constants, e.g., `Interop`. If [None] then [Self.class] is used
    #[builder(setter(into))]
    class_constants: Option<String>,
    /// DLL to load, e.g., `my_library`.
    #[builder(setter(into))]
    dll_name: String,
    /// Maps which namespace id belongs into which FQN (e.g., "common" => "MyCompany.Common").
    #[builder(setter(into))]
    namespace_mappings: NamespaceMappings,
    /// Namespace ID of _this_ namespace to write (default "").
    #[builder(setter(into))]
    namespace_id: String,
    /// Sets the visibility access modifiers for generated types.
    #[builder(setter(into))]
    pub(crate) visibility_types: Visibility,
    /// Which types to write.
    #[builder(setter(into))]
    write_types: WriteTypes,
    /// Also generate markers for easier debugging.
    debug: bool,
    /// Enrich user-provided item documentation with safety warnings and proper API use hints.
    doc_hints: bool,
    pub(crate) inventory: Inventory,
}

#[allow(clippy::unused_self)]
impl Interop {
    /// Creates a new [`InteropBuilder`].
    #[must_use]
    pub fn builder() -> InteropBuilder {
        InteropBuilder::new()
    }

    fn debug(&self, w: &mut IndentWriter, marker: &str) -> Result<(), Error> {
        if !self.debug {
            return Ok(());
        }

        indented!(w, r"// Debug - {} ", marker)
    }

    #[must_use]
    fn namespace_for_id(&self, id: &str) -> String {
        self.namespace_mappings
            .get(id)
            .unwrap_or_else(|| panic!("Found a namespace not mapped '{id}'. You should specify this one in the config."))
            .to_string()
    }

    pub(crate) fn inline_hint(&self, w: &mut IndentWriter, indents: usize) -> Result<(), Error> {
        for _ in 0..indents {
            w.indent();
        }

        indented!(w, r"[MethodImpl(MethodImplOptions.AggressiveOptimization)]")?;

        for _ in 0..indents {
            w.unindent();
        }

        Ok(())
    }

    #[must_use]
    #[allow(dead_code)] // TODO?
    fn should_emit_delegate(&self) -> bool {
        match self.write_types {
            WriteTypes::Namespace => false,
            WriteTypes::NamespaceAndInteroptopusGlobal => self.namespace_id.is_empty(),
            WriteTypes::All => true,
        }
    }

    #[must_use]
    #[allow(clippy::match_like_matches_macro)]
    pub fn should_emit_marshaller(&self, ctype: &Type) -> bool {
        match ctype {
            Type::Array(_) => true,
            Type::Composite(_) => true,
            _ => false,
        }
    }

    #[allow(dead_code)]
    fn has_emittable_marshallers(&self, types: &[Type]) -> bool {
        types.iter().any(|x| self.should_emit_marshaller(x))
    }

    fn has_emittable_functions(&self, functions: &[Function]) -> bool {
        functions.iter().any(|x| self.should_emit_by_meta(x.meta()))
    }

    #[must_use]
    fn has_emittable_constants(&self, constants: &[Constant]) -> bool {
        constants.iter().any(|x| self.should_emit_by_meta(x.meta()))
    }

    #[must_use]
    fn should_emit_by_meta(&self, meta: &Meta) -> bool {
        meta.module() == self.namespace_id
    }

    fn is_custom_marshalled(&self, x: &Type) -> bool {
        self.should_emit_marshaller(x)
            || match x {
                Type::FnPointer(y) => self.has_custom_marshalled_delegate(y.signature()),
                Type::Pattern(y) => match y {
                    TypePattern::NamedCallback(z) => self.has_custom_marshalled_delegate(z.fnpointer().signature()),
                    TypePattern::Slice(_) => true,
                    TypePattern::SliceMut(_) => true,
                    _ => false,
                },
                _ => false,
            }
    }

    fn has_custom_marshalled_types(&self, signature: &Signature) -> bool {
        let mut types = signature.params().iter().map(|x| x.the_type().clone()).collect::<Vec<_>>();
        types.push(signature.rval().clone());

        types.iter().any(|x| self.is_custom_marshalled(x))
    }

    fn has_custom_marshalled_delegate(&self, signature: &Signature) -> bool {
        let mut types = signature.params().iter().map(|x| x.the_type().clone()).collect::<Vec<_>>();
        types.push(signature.rval().clone());

        types.iter().any(|x| match x {
            Type::FnPointer(y) => self.has_custom_marshalled_types(y.signature()),
            Type::Pattern(TypePattern::NamedCallback(z)) => self.has_custom_marshalled_types(z.fnpointer().signature()),
            _ => false,
        })
    }

    fn to_native_callback_typespecifier(&self, t: &Type) -> String {
        match t {
            Type::Pattern(TypePattern::Slice(_)) => format!("{}.Unmanaged", param_to_type(t)),
            Type::Pattern(TypePattern::SliceMut(_)) => format!("{}.Unmanaged", param_to_type(t)),
            Type::Pattern(TypePattern::Utf8String(_)) => format!("{}.Unmanaged", param_to_type(t)),
            Type::Composite(_) => format!("{}.Unmanaged", param_to_type(t)),
            Type::Enum(_) => format!("{}.Unmanaged", param_to_type(t)),
            _ => param_to_type(t),
        }
    }

    #[allow(clippy::match_like_matches_macro)]
    fn has_overloadable(&self, signature: &Signature) -> bool {
        signature.params().iter().any(|x| match x.the_type() {
            Type::Pattern(p) => match p {
                TypePattern::NamedCallback(_) => true,
                TypePattern::AsyncCallback(_) => true,
                _ => false,
            },
            _ => false,
        })
    }

    /// Checks whether for the given type and the current file a type definition should be emitted.
    #[must_use]
    fn should_emit_by_type(&self, t: &Type) -> bool {
        if self.write_types == WriteTypes::All {
            return true;
        }

        if is_global_type(t) {
            return self.write_types == WriteTypes::NamespaceAndInteroptopusGlobal;
        }

        match t {
            Type::Primitive(_) => self.write_types == WriteTypes::NamespaceAndInteroptopusGlobal,
            Type::Array(_) => false,
            Type::Enum(x) => self.should_emit_by_meta(x.meta()),
            Type::Opaque(x) => self.should_emit_by_meta(x.meta()),
            Type::Composite(x) => self.should_emit_by_meta(x.meta()),
            Type::FnPointer(_) => true,
            Type::ReadPointer(_) => false,
            Type::ReadWritePointer(_) => false,
            Type::Pattern(x) => match x {
                TypePattern::CStrPointer => true,
                TypePattern::APIVersion => true,
                TypePattern::Slice(x) => self.should_emit_by_meta(x.meta()),
                TypePattern::SliceMut(x) => self.should_emit_by_meta(x.meta()),
                TypePattern::Option(x) => self.should_emit_by_meta(x.meta()),
                TypePattern::Result(x) => self.should_emit_by_meta(x.meta()),
                TypePattern::Bool => self.write_types == WriteTypes::NamespaceAndInteroptopusGlobal,
                TypePattern::CChar => false,
                TypePattern::NamedCallback(x) => self.should_emit_by_meta(x.meta()),
                TypePattern::AsyncCallback(x) => self.should_emit_by_meta(x.meta()),
                TypePattern::Vec(x) => self.should_emit_by_meta(x.meta()),
                TypePattern::Utf8String(_) => false,
            },
        }
    }

    fn write_all(&self, w: &mut IndentWriter) -> Result<(), Error> {
        write_file_header_comments(self, w)?;
        w.newline()?;

        write_imports(self, w)?;
        w.newline()?;

        write_namespace_context(self, w, |w| {
            if self.class_constants.is_none() || self.class_constants == Some(self.clone().class) {
                if self.has_emittable_functions(self.inventory.functions()) || self.has_emittable_constants(self.inventory.constants()) {
                    write_class_context(self, &self.class, w, |w| {
                        write_native_lib_string(self, w)?;
                        w.newline()?;

                        write_abi_guard(self, w)?;
                        w.newline()?;

                        write_pattern_async_trampoline_initializers(self, w)?;
                        w.newline()?;

                        write_constants(self, w)?;
                        w.newline()?;

                        write_functions(self, w)?;
                        Ok(())
                    })?;
                }
            } else {
                if self.has_emittable_constants(self.inventory.constants()) {
                    write_class_context(self, self.class_constants.as_ref().unwrap(), w, |w| {
                        write_constants(self, w)?;
                        w.newline()?;

                        Ok(())
                    })?;
                }

                if self.has_emittable_functions(self.inventory.functions()) {
                    w.newline()?;
                    write_class_context(self, &self.class, w, |w| {
                        write_native_lib_string(self, w)?;
                        w.newline()?;

                        write_abi_guard(self, w)?;
                        w.newline()?;

                        write_functions(self, w)?;
                        Ok(())
                    })?;
                }
            }

            w.newline()?;
            write_type_definitions(self, w)?;

            w.newline()?;
            write_patterns(self, w)?;

            w.newline()?;
            write_builtins(self, w)?;

            Ok(())
        })?;

        Ok(())
    }
}

impl Bindings for Interop {
    fn write_to(&self, w: &mut IndentWriter) -> Result<(), Error> {
        self.write_all(w)
    }
}

impl InteropBuilder {
    /// Creates a new builder instance, **start here**.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}
