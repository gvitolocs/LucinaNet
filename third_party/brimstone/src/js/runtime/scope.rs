use crate::runtime::{
    Context, EvalResult, Handle, HeapItemKind, HeapPtr, PropertyKey, Realm, Value,
    abstract_operations::has_property,
    alloc_error::AllocResult,
    boxed_value::BoxedValue,
    collections::InlineArray,
    error::{
        err_access_before_initialization, err_assign_constant, err_cannot_set_property,
        err_not_defined,
    },
    gc::{AnyHeapItem, HeapItem, HeapVisitor},
    get,
    module::source_text_module::SourceTextModule,
    object_value::ObjectValue,
    ordinary_object::ordinary_object_create,
    scope_names::ScopeNames,
    shape::Shape,
    string_value::StringValue,
    type_utilities::to_boolean,
};

#[repr(C)]
pub struct Scope {
    shape: HeapPtr<Shape>,
    /// The type of this scope
    kind: ScopeKind,
    /// Parent scope, forming a chain of scopes up to the global scope.
    parent: Option<HeapPtr<Scope>>,
    /// Names of the slots in this scope.
    scope_names: HeapPtr<ScopeNames>,
    /// Object containing scope bindings if this is a global or with scope.
    object: Option<HeapPtr<ObjectValue>>,
    /// Inline array of slots for variables in this scope.
    slots: InlineArray<Value>,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ScopeKind {
    Global,
    Module,
    Lexical,
    /// A function scope is treated as a var scope when looking up the containing var scope.
    Function,
    With,
}

impl Scope {
    #[inline]
    fn new(
        cx: Context,
        kind: ScopeKind,
        parent: Option<Handle<Scope>>,
        scope_names: Handle<ScopeNames>,
        object: Option<Handle<ObjectValue>>,
    ) -> AllocResult<Handle<Scope>> {
        let num_slots = scope_names.len();
        let size = Self::calculate_size_in_bytes(num_slots);
        let mut scope = cx.alloc_uninit_with_size::<Scope>(size)?;

        scope.shape = cx.shapes.get(HeapItemKind::Scope);
        scope.kind = kind;
        scope.parent = parent.map(|p| *p);
        scope.scope_names = *scope_names;
        scope.object = object.map(|o| *o);

        scope.slots.init_with(num_slots, Value::undefined());

        Ok(scope.to_handle())
    }

    pub fn new_global(
        cx: Context,
        scope_names: Handle<ScopeNames>,
        global_object: Handle<ObjectValue>,
    ) -> AllocResult<Handle<Scope>> {
        Self::new(cx, ScopeKind::Global, None, scope_names, Some(global_object))
    }

    pub fn new_module(
        cx: Context,
        scope_names: Handle<ScopeNames>,
        global_object: Handle<ObjectValue>,
    ) -> AllocResult<Handle<Scope>> {
        Self::new(cx, ScopeKind::Module, None, scope_names, Some(global_object))
    }

    pub fn new_lexical(
        cx: Context,
        parent: Handle<Scope>,
        scope_names: Handle<ScopeNames>,
    ) -> AllocResult<Handle<Scope>> {
        Self::new(cx, ScopeKind::Lexical, Some(parent), scope_names, None)
    }

    pub fn new_function(
        cx: Context,
        parent: Handle<Scope>,
        scope_names: Handle<ScopeNames>,
    ) -> AllocResult<Handle<Scope>> {
        Self::new(cx, ScopeKind::Function, Some(parent), scope_names, None)
    }

    pub fn new_with(
        cx: Context,
        parent: Handle<Scope>,
        scope_names: Handle<ScopeNames>,
        object: Handle<ObjectValue>,
    ) -> AllocResult<Handle<Scope>> {
        Self::new(cx, ScopeKind::With, Some(parent), scope_names, Some(object))
    }

    const SLOTS_OFFSET: usize = std::mem::offset_of!(Scope, slots);

    fn calculate_size_in_bytes(num_slots: usize) -> usize {
        Self::SLOTS_OFFSET + InlineArray::<Value>::calculate_size_in_bytes(num_slots)
    }

    #[inline]
    pub fn kind(&self) -> ScopeKind {
        self.kind
    }

    #[inline]
    pub fn parent(&self) -> Option<HeapPtr<Scope>> {
        self.parent
    }

    #[inline]
    pub fn parent_ptr(&self) -> HeapPtr<Scope> {
        self.parent.unwrap()
    }

    #[inline]
    pub fn scope_names_ptr(&self) -> HeapPtr<ScopeNames> {
        self.scope_names
    }

    #[inline]
    pub fn get_slot(&self, index: usize) -> Value {
        self.slots.as_slice()[index]
    }

    #[inline]
    pub fn get_slot_mut(&mut self, index: usize) -> &mut Value {
        &mut self.slots.as_mut_slice()[index]
    }

    /// Dynamically look up the `this` binding in this scope, walking the scope chain until found.
    pub fn lookup_this_for_eval(cx: Context, mut scope: HeapPtr<Self>) -> Value {
        loop {
            for (i, name_ptr) in scope.scope_names_ptr().name_ptrs().iter().enumerate() {
                if name_ptr.eq_str("this") {
                    return scope.get_slot(i);
                }
            }

            match scope.parent {
                Some(parent) => scope = parent,
                None => return cx.current_realm_ptr().global_object_ptr().as_value(),
            }
        }
    }

    #[inline]
    pub fn set_slot(&mut self, index: usize, value: Value) {
        self.slots.as_mut_slice()[index] = value;
    }

    /// Return the boxed value at the given slot index of a module scope.
    #[inline]
    pub fn get_module_slot(&self, index: usize) -> HeapPtr<BoxedValue> {
        let value = self.get_slot(index);

        debug_assert!(value.is::<BoxedValue>());

        value.as_pointer().cast::<BoxedValue>()
    }

    /// Set the boxed value at the given slot index of a module scope.
    pub fn set_heap_item_slot(&mut self, index: usize, heap_item: HeapPtr<AnyHeapItem>) {
        self.set_slot(index, Value::heap_item(heap_item));
    }

    /// Return the realm stored in this global scope.
    ///
    /// Should only be called on global scopes.
    #[inline]
    pub fn global_scope_realm(&self) -> HeapPtr<Realm> {
        debug_assert!(self.kind == ScopeKind::Global);
        self.get_slot(0).as_pointer().cast::<Realm>()
    }

    /// Return the source text module stored in this module scope, if one exists.
    ///
    /// Should only be called on module scopes.
    #[inline]
    pub fn module_scope_module(&self) -> Option<HeapPtr<SourceTextModule>> {
        debug_assert!(self.kind == ScopeKind::Module);

        self.get_slot(0).as_opt::<SourceTextModule>()
    }
}

impl Handle<Scope> {
    /// Create a new scope that is an exact duplicate of this scope.
    pub fn duplicate(&self, cx: Context) -> AllocResult<HeapPtr<Scope>> {
        let size = Scope::calculate_size_in_bytes(self.slots.len());
        let new_scope = cx.alloc_uninit_with_size::<Scope>(size)?;

        // Can copy the memory directly
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.as_ptr() as *const u8,
                new_scope.as_ptr() as *mut u8,
                size,
            )
        }

        Ok(new_scope)
    }

    /// Dynamically look up a name in this scope, walking the scope chain until found. The name must
    /// be an interned string.
    ///
    /// Perform validation such as TDZ checks.
    #[inline]
    pub fn lookup(
        &self,
        cx: Context,
        name: Handle<StringValue>,
        is_strict: bool,
    ) -> EvalResult<Option<Handle<Value>>> {
        // Reuse handles while walking scope chain
        let mut object_handle = Handle::<ObjectValue>::empty(cx);
        let mut scope = Handle::<Scope>::empty(cx);
        scope.replace(**self);

        loop {
            // First check inline slots using scope names table
            let scope_names = scope.scope_names_ptr();
            if let Some(index) = scope_names.lookup_name(*name.as_flat()) {
                let slot_value = scope.get_slot(index);

                // If found in a module scope then must check if loading a module binding, meaning
                // we need to load the value from the BoxedValue.
                let value =
                    if scope.kind == ScopeKind::Module && scope_names.is_module_binding(index) {
                        let boxed_value = slot_value.as_pointer().cast::<BoxedValue>();
                        boxed_value.get()
                    } else {
                        slot_value
                    };

                // Perform TDZ check since binding may not yet be initialized. The only exception is
                // for `this` in a derived constructor which performs a separate init check.
                if value.is_empty() && !scope_names.is_derived_constructor_this(index) {
                    return err_access_before_initialization(cx, name);
                }

                return Ok(Some(value.to_handle(cx)));
            }

            // Then check scope object if one exists
            if let Some(object) = scope.object {
                object_handle.replace(object);

                // Name is an interned string (and cannot be a number) so is already a property key
                let key = name.cast::<PropertyKey>();

                if scope.has_object_binding(cx, object_handle, key)? {
                    // Must check if property exists again in GetBindingValue
                    let still_exists = has_property(cx, object_handle, key)?;
                    if !still_exists {
                        if is_strict {
                            return err_not_defined(cx, name);
                        } else {
                            return Ok(Some(cx.undefined()));
                        }
                    }

                    let value = get(cx, object_handle, key)?;
                    return Ok(Some(value));
                }
            }

            // Otherwise move to parent scope
            if let Some(parent) = scope.parent.as_ref() {
                scope.replace(*parent);
            } else {
                // Finally check for global lexical names
                let realm = scope.global_scope_realm();
                let value = realm.get_lexical_name(cx, *name.as_flat())?;

                return Ok(value.map(|v| v.to_handle(cx)));
            }
        }
    }

    /// Dynamically store a name in this scope, walking the scope chain until found. Return whether
    /// the name was found. The name must be an interned string.
    ///
    /// Perform validation such as TDZ and immutability checks.
    #[inline]
    pub fn lookup_store(
        &mut self,
        cx: Context,
        name: Handle<StringValue>,
        value: Handle<Value>,
        is_strict: bool,
    ) -> EvalResult<bool> {
        // Reuse handles while walking scope chain
        let mut object_handle = Handle::<ObjectValue>::empty(cx);
        let mut scope = Handle::<Scope>::empty(cx);
        scope.replace(**self);

        loop {
            // First check inline slots using scope names table
            let scope_names = scope.scope_names_ptr();
            if let Some(index) = scope_names.lookup_name(*name.as_flat()) {
                // Perform TDZ check since binding may not yet be initialized. The only exception is
                // for `this` in a derived constructor which performs a separate init check.
                if scope.get_slot(index).is_empty()
                    && !scope_names.is_derived_constructor_this(index)
                {
                    return err_access_before_initialization(cx, name);
                }

                // If found in a module scope then must check if storing to a module binding,
                // meaning we need to store the value in the BoxedValue.
                if scope.kind == ScopeKind::Module && scope_names.is_module_binding(index) {
                    let mut boxed_value = scope.get_module_slot(index);

                    // Perform TDZ check since binding may not yet be initialized. Note that import
                    // bindings are created as initialized local bindings - the value here actually
                    // tracks whether the binding within the source module has been initialized.
                    if boxed_value.get().is_empty() && !scope_names.is_import_binding(index) {
                        return err_access_before_initialization(cx, name);
                    }

                    // Check if storing to a constant
                    if scope_names.is_immutable(index) {
                        return err_assign_constant(cx, *name.as_flat());
                    }

                    boxed_value.set(*value);
                    return Ok(true);
                }

                // Check if storing to a constant
                if scope_names.is_immutable(index) {
                    return err_assign_constant(cx, *name.as_flat());
                }

                // Check if storing to a function expression name
                if scope_names.is_function_expression_name(index) {
                    // Error if reassigning in strict mode, otherwise is a no-op
                    if is_strict {
                        return err_assign_constant(cx, *name.as_flat());
                    } else {
                        return Ok(true);
                    }
                }

                scope.set_slot(index, *value);
                return Ok(true);
            }

            // Then check scope object if one exists
            if let Some(object) = scope.object {
                object_handle.replace(object);

                // Name is an interned string (and cannot be a number) so is already a property key
                let key = name.cast::<PropertyKey>();

                if scope.has_object_binding(cx, object_handle, key)? {
                    // Must check if property exists again in SetMutableBinding
                    let still_exists = has_property(cx, object_handle, key)?;
                    if !still_exists && is_strict {
                        return err_not_defined(cx, name);
                    }

                    let success = object_handle.set(cx, key, value, object_handle.into())?;
                    if !success && is_strict {
                        return err_cannot_set_property(cx, name.format()?);
                    }

                    // Name was found, even if the set failed
                    return Ok(true);
                }
            }

            // Otherwise move to parent scope
            if let Some(parent) = scope.parent.as_ref() {
                scope.replace(*parent);
            } else {
                // Otherwise check for a global lexical name
                let mut realm = scope.global_scope_realm();
                let success = realm.set_lexical_name(cx, *name.as_flat(), *value)?;

                return Ok(success);
            }
        }
    }

    /// Resolve a name to the environment record that holds its binding, walking the scope chain in
    /// the same order as `lookup` but without loading or checking any value. Used to create the
    /// reference of a left hand side expression, which PutValue must still use once the right hand
    /// side of an assignment has run and may have deleted, replaced or shadowed the binding
    /// (ES 2024 6.2.5.6 PutValue step 5, and the steps named in the code below).
    ///
    /// No TDZ or immutability check is performed here: resolving a name only chooses the
    /// environment record, and both checks belong to GetValue and PutValue.
    ///
    /// The name must be an interned string.
    #[inline]
    pub fn lookup_reference(
        &mut self,
        cx: Context,
        name: Handle<StringValue>,
    ) -> EvalResult<Option<Handle<Scope>>> {
        // Reuse handles while walking scope chain
        let mut object_handle = Handle::<ObjectValue>::empty(cx);
        let mut scope = Handle::<Scope>::empty(cx);
        scope.replace(**self);

        loop {
            // First check inline slots using scope names table
            let scope_names = scope.scope_names_ptr();
            if scope_names.lookup_name(*name.as_flat()).is_some() {
                return Ok(Some(scope));
            }

            // Then check scope object if one exists
            if let Some(object) = scope.object {
                object_handle.replace(object);

                // Name is an interned string (and cannot be a number) so is already a property key
                let key = name.cast::<PropertyKey>();

                if scope.has_object_binding(cx, object_handle, key)? {
                    return Ok(Some(scope));
                }
            }

            // Otherwise move to parent scope
            if let Some(parent) = scope.parent.as_ref() {
                scope.replace(*parent);
            } else {
                // Finally check for global lexical names, whose environment record is a global
                // scope (ES 2024 9.1.1.4.1 Global Environment Record HasBinding step 3)
                let realm = scope.global_scope_realm();
                if realm.get_lexical_name(cx, *name.as_flat())?.is_some() {
                    return Ok(Some(scope));
                }

                return Ok(None);
            }
        }
    }

    /// GetValue for a reference whose base is the given environment record, without walking the
    /// scope chain any further: Declarative Environment Record GetBindingValue, Object Environment
    /// Record GetBindingValue steps 1-4 (ES 2024 9.1.1.2.6) and Global Environment Record
    /// GetBindingValue (ES 2024 9.1.1.4.6), which looks at its lexical declarations before the
    /// global object.
    ///
    /// The name must be an interned string.
    #[inline]
    pub fn load_from_reference(
        &mut self,
        cx: Context,
        name: Handle<StringValue>,
        is_strict: bool,
    ) -> EvalResult<Value> {
        let scope_names = self.scope_names_ptr();

        // Bindings in scope slots cannot disappear, so the slot found here is the binding that the
        // reference was created for.
        if let Some(index) = scope_names.lookup_name(*name.as_flat()) {
            let slot_value = self.get_slot(index);

            // If found in a module scope then must check if loading a module binding, meaning
            // we need to load the value from the BoxedValue.
            if self.kind == ScopeKind::Module && scope_names.is_module_binding(index) {
                let boxed_value = slot_value.as_pointer().cast::<BoxedValue>();
                return Ok(boxed_value.get());
            }

            // Perform TDZ check since binding may not yet be initialized. The only exception is
            // for `this` in a derived constructor which performs a separate init check.
            if slot_value.is_empty() && !scope_names.is_derived_constructor_this(index) {
                return err_access_before_initialization(cx, name);
            }

            return Ok(slot_value);
        }

        // The lexical declarations of a global record are held in the realm rather than as
        // properties of the global object, and they come first (ES 2024 9.1.1.4.6 steps 2-3)
        if self.kind == ScopeKind::Global {
            let realm = self.global_scope_realm();
            if let Some(value) = realm.get_lexical_name(cx, *name.as_flat())? {
                return Ok(value);
            }
        }

        // An object binding may have been deleted after the reference was created. The unscopables
        // check is not repeated because the base of the reference is already fixed, and Object
        // Environment Record GetBindingValue steps 2-3 give the empty value for a deleted property.
        if let Some(object) = self.object {
            let object_handle = object.to_handle();

            // Name is an interned string (and cannot be a number) so is already a property key
            let key = name.cast::<PropertyKey>();

            if !has_property(cx, object_handle, key)? {
                return if is_strict {
                    err_not_defined(cx, name)
                } else {
                    Ok(*cx.undefined())
                };
            }

            let value = get(cx, object_handle, key)?;
            return Ok(*value);
        }

        // Only reachable if the binding was removed from a record that cannot lose bindings, so
        // report the name as unbound rather than writing somewhere else.
        if is_strict {
            err_not_defined(cx, name)
        } else {
            Ok(*cx.undefined())
        }
    }

    /// PutValue for a reference whose base is the given environment record, without walking the
    /// scope chain any further: Declarative Environment Record SetMutableBinding, Object
    /// Environment Record SetMutableBinding steps 1-5 (ES 2024 9.1.1.2.5) and Global Environment
    /// Record SetMutableBinding (ES 2024 9.1.1.4.5), which looks at its lexical declarations before
    /// the global object.
    ///
    /// Return false if the record holds no binding for the name at all, which is the case for a
    /// reference whose base was never found; the caller then performs the PutValue handling of an
    /// unresolvable reference.
    ///
    /// The name must be an interned string.
    #[inline]
    pub fn store_to_reference(
        &mut self,
        cx: Context,
        name: Handle<StringValue>,
        value: Handle<Value>,
        is_strict: bool,
    ) -> EvalResult<bool> {
        let scope_names = self.scope_names_ptr();

        if let Some(index) = scope_names.lookup_name(*name.as_flat()) {
            // Perform TDZ check since binding may not yet be initialized. The only exception is
            // for `this` in a derived constructor which performs a separate init check.
            if self.get_slot(index).is_empty() && !scope_names.is_derived_constructor_this(index) {
                return err_access_before_initialization(cx, name);
            }

            // If found in a module scope then must check if storing to a module binding, meaning
            // we need to store the value in the BoxedValue.
            if self.kind == ScopeKind::Module && scope_names.is_module_binding(index) {
                let mut boxed_value = self.get_module_slot(index);

                // Perform TDZ check since binding may not yet be initialized. Note that import
                // bindings are created as initialized local bindings - the value here actually
                // tracks whether the binding within the source module has been initialized.
                if boxed_value.get().is_empty() && !scope_names.is_import_binding(index) {
                    return err_access_before_initialization(cx, name);
                }

                // Check if storing to a constant
                if scope_names.is_immutable(index) {
                    return err_assign_constant(cx, *name.as_flat());
                }

                boxed_value.set(*value);
                return Ok(true);
            }

            // Check if storing to a constant
            if scope_names.is_immutable(index) {
                return err_assign_constant(cx, *name.as_flat());
            }

            // Check if storing to a function expression name
            if scope_names.is_function_expression_name(index) {
                // Error if reassigning in strict mode, otherwise is a no-op
                if is_strict {
                    return err_assign_constant(cx, *name.as_flat());
                } else {
                    return Ok(true);
                }
            }

            self.set_slot(index, *value);
            return Ok(true);
        }

        // The lexical declarations of a global record are held in the realm rather than as
        // properties of the global object, and they come first (ES 2024 9.1.1.4.5 steps 2-4)
        if self.kind == ScopeKind::Global {
            let mut realm = self.global_scope_realm();
            if realm.set_lexical_name(cx, *name.as_flat(), *value)? {
                return Ok(true);
            }
        }

        // A property that was deleted after the reference was created is still assigned to this same
        // binding object: Object Environment Record SetMutableBinding steps 2-4 (ES 2024 9.1.1.2.5)
        // make that a ReferenceError when the assignment is strict, and otherwise leave it to Set,
        // which defines the property again.
        if let Some(object) = self.object {
            let mut object_handle = object.to_handle();

            // Name is an interned string (and cannot be a number) so is already a property key
            let key = name.cast::<PropertyKey>();

            let still_exists = has_property(cx, object_handle, key)?;
            if !still_exists && is_strict {
                return err_not_defined(cx, name);
            }

            let success = object_handle.set(cx, key, value, object_handle.into())?;
            if !success && is_strict {
                return err_cannot_set_property(cx, name.format()?);
            }

            return Ok(true);
        }

        Ok(false)
    }

    /// SetMutableBinding(name, value, false) on this scope as a VariableEnvironment, without
    /// walking the scope chain. Used when a block-level FunctionDeclaration is evaluated under
    /// B.3.2, after its var binding was created (FunctionDeclarationInstantiation step 32.a.i.2.c,
    /// GlobalDeclarationInstantiation step 12.b.ii.2.a.ii.3, EvalDeclarationInstantiation step
    /// 13.b.ii.7.b).
    ///
    /// The name must be an interned string.
    pub fn set_var_scope_binding(
        &mut self,
        cx: Context,
        name: Handle<StringValue>,
        value: Handle<Value>,
    ) -> EvalResult<()> {
        let flat_name = *name.as_flat();

        if self.kind == ScopeKind::Global {
            // A global lexical declaration of the name (from any script) means the var binding was
            // not created (GlobalDeclarationInstantiation step 12.b.ii.2.a)
            if self.global_scope_realm().has_lexical_name(flat_name) {
                return Ok(());
            }
        } else {
            let scope_names = self.scope_names_ptr();
            if let Some(index) = scope_names.lookup_name(flat_name) {
                // A lexical name of a var scope belongs to the function's top level lexical
                // environment, so the var binding was not created (EvalDeclarationInstantiation
                // step 13.b.ii.4)
                if !scope_names.is_lexical(index) {
                    self.set_slot(index, *value);
                }

                return Ok(());
            }
        }

        // Otherwise the binding is a property of the scope object: the global object, or the
        // object holding the vars created by sloppy direct evals. For an object Environment Record
        // SetMutableBinding performs ? Set(bindingObject, N, V, S) with S false, so a failed set is
        // ignored.
        let mut object = self.ensure_scope_object(cx)?;
        let key = name.cast::<PropertyKey>();
        object.set(cx, key, value, object.into())?;

        Ok(())
    }

    /// Dynamically delete a binding in this scope, walking the scope chain until found. Return
    /// false only when we failed to delete an own non-configurable property, otherwise return true.
    ///
    /// The name must be an interned string.
    #[inline]
    pub fn lookup_delete(&mut self, cx: Context, name: Handle<StringValue>) -> EvalResult<bool> {
        // Reuse handles while walking scope chain
        let mut object_handle = Handle::<ObjectValue>::empty(cx);
        let mut scope = Handle::<Scope>::empty(cx);
        scope.replace(**self);

        loop {
            // Properties can be deleted off scope objects. This can be either the global object,
            // with target object, or vars added in a sloppy eval.
            if let Some(object) = scope.object {
                object_handle.replace(object);

                // Name is an interned string (and cannot be a number) so is already a property key
                let key = name.cast::<PropertyKey>();

                // If the property is found, delete it
                if has_property(cx, object_handle, key)? {
                    return object_handle.delete(cx, key);
                }
            }

            // Any bindings stored in VM scope slots cannot be deleted. The only deletable bindings
            // are vars and functions in eval scopes or dynamically created vars.
            if scope
                .scope_names_ptr()
                .lookup_name(*name.as_flat())
                .is_some()
            {
                return Ok(false);
            }

            // Move to parent scope
            if let Some(parent) = scope.parent.as_ref() {
                scope.replace(*parent);
            } else {
                return Ok(true);
            }
        }
    }

    /// For scopes with an object, return whether the key is a binding in the object.
    ///
    /// For with statements, we must ignore keys that are in object[Symbol.unscopables].
    fn has_object_binding(
        &self,
        cx: Context,
        object: Handle<ObjectValue>,
        key: Handle<PropertyKey>,
    ) -> EvalResult<bool> {
        // Check if key appears in object
        if !has_property(cx, object, key)? {
            return Ok(false);
        }

        if self.kind != ScopeKind::With {
            return Ok(true);
        }

        // With statements must also ignore properties in @@unscopables
        let unscopables_key = cx.symbols.unscopables();
        let unscopables = get(cx, object, unscopables_key)?;
        if unscopables.is_object() {
            let unscopables = unscopables.as_object();

            let value = get(cx, unscopables, key)?;
            let blocked = to_boolean(*value);
            if blocked {
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// Return the object for this scope, creating it if necessary.
    pub fn ensure_scope_object(&mut self, cx: Context) -> AllocResult<Handle<ObjectValue>> {
        if let Some(object) = self.object {
            return Ok(object.to_handle());
        }

        let object = ordinary_object_create(cx)?;
        self.object = Some(*object);
        Ok(object)
    }
}

impl HeapItem for Scope {
    fn byte_size(scope: HeapPtr<Self>) -> usize {
        Scope::calculate_size_in_bytes(scope.slots.len())
    }

    fn visit_pointers(mut scope: HeapPtr<Self>, visitor: &mut impl HeapVisitor) {
        visitor.visit_pointer(&mut scope.shape);
        visitor.visit_pointer_opt(&mut scope.parent);
        visitor.visit_pointer(&mut scope.scope_names);
        visitor.visit_pointer_opt(&mut scope.object);

        for slot in scope.slots.as_mut_slice() {
            visitor.visit_value(slot);
        }
    }
}
