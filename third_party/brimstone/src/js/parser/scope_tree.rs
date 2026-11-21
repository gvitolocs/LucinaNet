use std::{cell::Cell, rc::Rc, sync::LazyLock};

use allocator_api2::alloc::Global;

use crate::{
    common::{
        alloc,
        options::Options,
        wtf_8::{Wtf8Str, Wtf8String},
    },
    parser::{
        ParseError,
        ast::{
            self, AstAlloc, AstBox, AstHashSet, AstIndexMap, AstPtr, AstSlice, AstSliceBuilder,
            AstStr, AstVec, FunctionId, TaggedResolvedScope,
        },
        loc::{Loc, Pos},
        reparse::ReparseInfo,
    },
};

pub struct ScopeTree<'a> {
    #[allow(clippy::vec_box)]
    /// Indexed by scope node id. None for a node removed by `compact_into` (no lazy function
    /// that has not run yet is nested in it).
    ast_nodes: AstVec<'a, Option<AstBox<'a, AstScopeNode<'a>>>>,
    vm_nodes: AstVec<'a, VMScopeNode<'a>>,
    current_node_id: ScopeNodeId,
    alloc: AstAlloc<'a>,
    options: Rc<Options>,
    /// Block-level function declarations whose var hoist target has not been finished yet (see
    /// `record_annex_b_block_function`), in source order.
    annex_b_pending: Vec<AnnexBPendingFunction<'a>>,
    /// Set by the parser while it adds the name of a sloppy plain FunctionDeclaration, which may
    /// redeclare another FunctionDeclaration in the same statement list (14.2.1, 14.12.1).
    annex_b_function_redeclaration: bool,
}

pub struct SavedScopeTreeState {
    current_node_id: ScopeNodeId,
    current_node_num_bindings: usize,
    num_nodes: usize,
    num_annex_b_pending: usize,
}

/// A block-level FunctionDeclaration seen by the parser whose var hoist target is not finished.
struct AnnexBPendingFunction<'a> {
    name: AstStr<'a>,
    /// The Block or Switch scope whose statement list contains the declaration.
    decl_scope: ScopeNodeId,
    /// The var hoist target (function, function body, script or eval scope) of `decl_scope`.
    hoist_target: ScopeNodeId,
    /// False for a labelled declaration, which is not "directly contained" in the statement list
    /// but still declares the name lexically there.
    eligible: bool,
}

type AddBindingResult<'a> = Result<AstPtr<AstScopeNode<'a>>, ParseError>;

/// Functions for constructing, mutating, and querying the AST scope tree.
impl<'a> ScopeTree<'a> {
    fn new_with_root(
        options: Rc<Options>,
        kind: ScopeNodeKind,
        alloc: AstAlloc<'a>,
    ) -> ScopeTree<'a> {
        let mut scope_tree = ScopeTree {
            ast_nodes: alloc::vec![in alloc],
            vm_nodes: alloc::vec![in alloc],
            current_node_id: INITIAL_SCOPE_ID,
            alloc,
            options,
            annex_b_pending: vec![],
            annex_b_function_redeclaration: false,
        };

        // Push the global scope node
        scope_tree.push_new_ast_scope_node(INITIAL_SCOPE_ID, kind, None);

        scope_tree
    }

    pub fn new_global(options: Rc<Options>, alloc: AstAlloc<'a>) -> ScopeTree<'a> {
        let mut scope_tree = Self::new_with_root(options, ScopeNodeKind::Global, alloc);
        scope_tree.add_implicit_this_binding(/* in_derived_constructor */ false);
        scope_tree
    }

    pub fn new_module(options: Rc<Options>, alloc: AstAlloc<'a>) -> ScopeTree<'a> {
        let mut scope_tree = Self::new_with_root(options, ScopeNodeKind::Module, alloc);
        scope_tree.add_implicit_this_binding(/* in_derived_constructor */ false);
        scope_tree
    }

    pub fn new_eval(
        options: Rc<Options>,
        is_direct: bool,
        in_derived_constructor: bool,
        alloc: AstAlloc<'a>,
    ) -> ScopeTree<'a> {
        // Start off without setting the strict flag. This flag will be set right away during parsing.
        let mut scope_tree = Self::new_with_root(
            options,
            ScopeNodeKind::Eval { is_direct, is_strict: false },
            alloc,
        );

        // Direct eval in derived constructor has no `this` binding so that `this` is resolved to a
        // dynamic lookup.
        if !in_derived_constructor {
            scope_tree.add_implicit_this_binding(/* in_derived_constructor */ false);
        }

        scope_tree
    }

    /// Move the finished scope tree into a box in its own arena, so that it does not depend on the
    /// AST arena.
    pub fn into_box(self) -> AstBox<'a, ScopeTree<'a>> {
        let alloc = self.alloc;
        AstBox::new_in(self, alloc)
    }

    /// A copy of a binding name in the scope tree's arena. Names from the parser may live in the
    /// AST arena (identifiers with escapes or non-ASCII code points), which is dropped before the
    /// scope tree when lazy functions are re-parsed.
    fn own_name(&self, name: AstStr<'a>) -> AstStr<'a> {
        let bytes = self.alloc.alloc_slice_copy(name.as_bytes());
        Wtf8Str::from_bytes_unchecked(bytes)
    }

    pub fn add_implicit_this_binding(&mut self, in_derived_constructor: bool) {
        self.add_binding(&THIS_NAME, BindingKind::new_implicit_this(in_derived_constructor))
            .unwrap();
    }

    pub fn finish_ast_scope_tree(mut self) -> ScopeTree<'a> {
        // The script or eval scope is never exited, so its block-level functions are decided here
        while let Some(pending) = self.annex_b_pending.last() {
            let hoist_target = pending.hoist_target;
            self.apply_annex_b_block_functions(hoist_target);
        }

        for node in self.ast_nodes.iter_mut().flatten() {
            // Re-sort bindings by index to properly order duplicates
            if node.has_duplicates {
                node.bindings.sort_by(|_, a, _, b| a.index.cmp(&b.index));
            }
        }

        self
    }

    /// Save the current state of the AST scope tree, so that it can be restored later.
    pub fn save(&self) -> SavedScopeTreeState {
        SavedScopeTreeState {
            current_node_id: self.current_node_id,
            current_node_num_bindings: self.get_ast_node(self.current_node_id).bindings.len(),
            num_nodes: self.ast_nodes.len(),
            num_annex_b_pending: self.annex_b_pending.len(),
        }
    }

    /// Restore from a saved state of the AST scope tree.
    pub fn restore(&mut self, saved_state: &SavedScopeTreeState) {
        self.current_node_id = saved_state.current_node_id;
        self.get_ast_node_mut(self.current_node_id)
            .bindings
            .truncate(saved_state.current_node_num_bindings);
        self.ast_nodes.truncate(saved_state.num_nodes);
        self.annex_b_pending
            .truncate(saved_state.num_annex_b_pending);
    }

    /// Enter a new AST scope node with the provided kind.
    pub fn enter_scope(&mut self, kind: ScopeNodeKind) -> AstPtr<AstScopeNode<'a>> {
        let node_id = self.ast_nodes.len();
        self.push_new_ast_scope_node(node_id, kind, Some(self.current_node_id));

        self.current_node_id = node_id;

        self.get_ast_node_ptr(node_id)
    }

    /// Exit the current AST scope node.
    pub fn exit_scope(&mut self) {
        // All declarations of a finished var hoist target are known, so its block-level functions
        // can be decided before the analysis resolves any use.
        let node_id = self.current_node_id;
        if self.get_ast_node(node_id).kind.is_hoist_target() {
            self.apply_annex_b_block_functions(node_id);
        }

        self.current_node_id = self.get_ast_node(node_id).parent.unwrap();
    }

    pub fn current_scope(&self) -> AstPtr<AstScopeNode<'a>> {
        AstPtr::from_ref(self.get_ast_node(self.current_node_id))
    }

    pub fn set_current_scope(&mut self, node_id: ScopeNodeId) {
        self.current_node_id = node_id;
    }

    fn get_ast_node(&self, node_id: ScopeNodeId) -> &AstScopeNode<'a> {
        self.ast_nodes[node_id]
            .as_deref()
            .expect("scope node removed by compaction")
    }

    fn get_ast_node_mut(&mut self, node_id: ScopeNodeId) -> &mut AstScopeNode<'a> {
        self.ast_nodes[node_id]
            .as_deref_mut()
            .expect("scope node removed by compaction")
    }

    pub fn get_ast_node_ptr(&self, node_id: ScopeNodeId) -> AstPtr<AstScopeNode<'a>> {
        AstPtr::from_ref(self.get_ast_node(node_id))
    }

    /// Record what is needed to parse the function of a function scope node again on its own.
    pub fn set_reparse_info(&mut self, node_id: ScopeNodeId, info: ReparseInfo) {
        let info = AstBox::new_in(info, self.alloc);
        self.get_ast_node_mut(node_id).reparse = Some(info);
    }

    /// The scope node with the given id.
    pub fn ast_node(&self, node_id: ScopeNodeId) -> &AstScopeNode<'a> {
        self.get_ast_node(node_id)
    }

    /// Whether the scope node with the given id still exists (see `compact_into`).
    pub fn has_ast_node(&self, node_id: ScopeNodeId) -> bool {
        self.ast_nodes
            .get(node_id)
            .is_some_and(|node| node.is_some())
    }

    /// A copy of the tree in `alloc` with only the nodes marked in `live` (its length is the number
    /// of nodes); the others are removed, and the VM nodes only they used are emptied. Ids stay as
    /// they are. Used once a program's AST is dropped, to keep only the scopes that lazy functions
    /// still to be parsed again are nested in (a live node's parent must be live too).
    pub fn compact_into<'b>(&self, alloc: AstAlloc<'b>, live: &[bool]) -> ScopeTree<'b> {
        let copy_name = |name: &Wtf8Str| -> AstStr<'b> {
            Wtf8Str::from_bytes_unchecked(alloc.alloc_slice_copy(name.as_bytes()))
        };

        let mut live_vm = vec![false; self.vm_nodes.len()];
        let mut ast_nodes = alloc::vec![in alloc];
        ast_nodes.reserve(self.ast_nodes.len());
        for (id, slot) in self.ast_nodes.iter().enumerate() {
            match slot {
                Some(node) if live[id] => {
                    debug_assert!(node.parent.is_none_or(|parent| live[parent]));
                    if let Some(vm_id) = node.vm_scope {
                        live_vm[vm_id] = true;
                    }
                    let copy = node.copy_into(alloc, &copy_name);
                    ast_nodes.push(Some(AstBox::new_in(copy, alloc)));
                }
                _ => ast_nodes.push(None),
            }
        }

        let mut vm_nodes = alloc::vec![in alloc];
        vm_nodes.reserve(self.vm_nodes.len());
        for (id, vm_node) in self.vm_nodes.iter().enumerate() {
            let mut bindings = AstSliceBuilder::new(alloc::vec![in alloc]);
            if live_vm[id] {
                for name in vm_node.bindings() {
                    bindings.push(copy_name(name));
                }
            }
            vm_nodes.push(VMScopeNode { bindings: bindings.build() });
        }

        ScopeTree {
            ast_nodes,
            vm_nodes,
            current_node_id: self.current_node_id,
            alloc,
            options: self.options.clone(),
            annex_b_pending: vec![],
            annex_b_function_redeclaration: false,
        }
    }

    /// Number of AST scope nodes.
    pub fn num_ast_nodes(&self) -> usize {
        self.ast_nodes.len()
    }

    pub fn get_vm_node(&self, node_id: ScopeNodeId) -> &VMScopeNode<'a> {
        &self.vm_nodes[node_id]
    }

    fn push_new_ast_scope_node(
        &mut self,
        id: ScopeNodeId,
        kind: ScopeNodeKind,
        parent: Option<ScopeNodeId>,
    ) {
        let owner_scope = if let ScopeNodeKind::Global
        | ScopeNodeKind::Module
        | ScopeNodeKind::Function { .. }
        | ScopeNodeKind::Eval { .. } = kind
        {
            // Global, module, function, and eval nodes are their own owner scopes
            id
        } else {
            // Otherwise inherit owner from parent scope
            self.get_ast_node(parent.unwrap()).owner_scope
        };

        let mut extra_var_names = AstHashSet::new_in(self.alloc);

        // Function body scopes must know the names of all parameter names in the parent function
        // scope (to check for conflicts against lexical declarations).
        if kind == ScopeNodeKind::FunctionBody {
            let func_scope = self.get_ast_node(parent.unwrap());
            // All bindings in function scope are guaranteed to be function parameters if a
            // function body scope is created.
            for (name, _) in func_scope.iter_bindings() {
                extra_var_names.insert(*name);
            }
        }

        // With scope nodes always have dynamic bindings
        let supports_dynamic_bindings = kind == ScopeNodeKind::With;

        let scope_node = AstScopeNode {
            id,
            parent,
            kind,
            bindings: AstIndexMap::new_in(self.alloc),
            extra_var_names,
            num_bindings: 0,
            has_duplicates: false,
            supports_dynamic_access: false,
            supports_dynamic_bindings,
            allow_empty_vm_node: false,
            may_dynamically_access_uninitialized_parameters: false,
            owner_scope,
            owned_scopes: alloc::vec![in self.alloc],
            num_local_registers: 0,
            vm_scope: None,
            reparse: None,
            annex_b_functions: alloc::vec![in self.alloc],
            catch_parameter_is_binding_identifier: false,
        };

        self.ast_nodes
            .push(Some(AstBox::new_in(scope_node, self.alloc)));
    }

    /// Add a binding to the AST scope tree, hoisting to a higher scope if necessary. On success
    /// return the AST scope node that the binding was added to.
    pub fn add_binding(&mut self, name: AstStr<'a>, kind: BindingKind<'a>) -> AddBindingResult<'a> {
        let name = self.own_name(name);
        if kind.is_lexically_scoped() {
            self.add_lexically_scoped_binding(name, kind)
        } else {
            self.add_var_scoped_binding(name, kind)
        }
    }

    fn add_lexically_scoped_binding(
        &mut self,
        name: AstStr<'a>,
        kind: BindingKind<'a>,
    ) -> AddBindingResult<'a> {
        let allow_function_redeclaration = self.annex_b_function_redeclaration;
        let node = self.get_ast_node_mut(self.current_node_id);

        if let Some(err) = node.error_if_lexical_name_already_declared(name) {
            // 14.2.1 / 14.12.1 (B.3.2): in sloppy code duplicate entries in the
            // LexicallyDeclaredNames of a statement list are allowed if they are only bound by
            // FunctionDeclarations. The binding then refers to the last declaration, which is the
            // one BlockDeclarationInstantiation step 3.b.iii leaves in it.
            if !(allow_function_redeclaration && node.is_function_declaration_name(name)) {
                return Err(err);
            }
        }

        // Lexical bindings in a switch always need a TDZ check since it hard to analyze when
        // the check can be elided. (e.g. due to out of order default case, or definitions and
        // uses in different cases).
        let needs_tdz_check = node.kind == ScopeNodeKind::Switch;

        node.add_binding(name, kind, None, needs_tdz_check);

        // Lexically scoped bindings in the global scope must be placed in a VM scope node so that
        // they can be accessed by different scripts.
        if node.kind == ScopeNodeKind::Global {
            let binding = node.bindings.get_mut(name).unwrap();
            binding.force_vm_scope = true;
        }

        Ok(self.get_ast_node_ptr(self.current_node_id))
    }

    fn add_var_scoped_binding(
        &mut self,
        name: AstStr<'a>,
        mut kind: BindingKind<'a>,
    ) -> AddBindingResult<'a> {
        // Walk up to the hoist target scope, checking for conflicting lexical bindings
        let mut node_id = self.current_node_id;
        let mut in_with_statement = false;
        let in_annex_b_mode = self.options.annex_b;

        // Scope node id to return, overriding the scope node where the binding is actually added
        let mut node_id_override = None;

        loop {
            let node = self.get_ast_node_mut(node_id);

            if let Some(binding) = node.bindings.get(name) {
                // In Annex B mode var declarations are allowed to be declared in a catch block with
                // a catch parameter of the same name.
                //
                // We allow the var declaration to propagate up to its hoist target. All references
                // to this name within the catch block will reference the catch parameter, including
                // the initialization within the var declaration itself. To achieve this we return
                // the catch scope node as the node where the var binding was added while still
                // adding the var binding to its hoist target scope higher up the scope chain.
                if in_annex_b_mode
                    && binding.kind.is_catch_parameter()
                    && matches!(kind, BindingKind::Var)
                {
                    node_id_override = Some(node_id);
                } else if binding.kind.is_lexically_scoped() {
                    // Error if there is already a lexically scoped binding with this name. Existing var
                    // scoped bindings with the same name are allowed.
                    return Err(ParseError::new_name_redeclaration(
                        name.to_owned_in(Global),
                        binding.kind.clone(),
                    ));
                }
            }

            // Track if the var binding is within a with statement
            if node.kind == ScopeNodeKind::With {
                in_with_statement = true;
            }

            if node.kind.is_hoist_target() {
                let is_body_var_arguments = node.kind == ScopeNodeKind::FunctionBody
                    && name == ARGUMENTS_NAME.as_str()
                    && matches!(kind, BindingKind::Var);

                let existing_binding = node.bindings.get(name);
                let can_overwrite =
                    // Function declarations overwrite all other bindings
                    kind.is_function()
                    // Function parameters with the same name overwrite earlier parameters
                    || Self::can_overwrite_function_binding(&existing_binding, &kind);

                if existing_binding.is_none() || can_overwrite {
                    // A function declaration that overwrites a function parameter shares a single
                    // binding with that parameter, so it must inherit the parameter's slot.
                    if let BindingKind::Function { parameter_index, .. } = &mut kind {
                        *parameter_index =
                            existing_binding.and_then(|binding| binding.kind().parameter_index());
                    }

                    let vm_location = if in_with_statement {
                        // Var bindings in a with statement require a dynamic lookup at runtime as
                        // they may reference a true var or a property of the with object.
                        Some(VMLocation::WithVar)
                    } else if node.kind == ScopeNodeKind::Global && !kind.is_implicit_this() {
                        // Var bindings in the global scope are immediately known to be global
                        Some(VMLocation::Global)
                    } else if matches!(node.kind, ScopeNodeKind::Eval { is_strict: false, .. })
                        && !kind.is_implicit_this()
                    {
                        // Var bindings in a sloppy eval are dynamically added to their parent
                        // var scope, and require a dynamic lookup at runtime.
                        Some(VMLocation::EvalVar)
                    } else {
                        None
                    };

                    node.add_binding(name, kind, vm_location, /* needs_tdz_check */ false);
                } else if existing_binding.is_some() && in_with_statement {
                    // Should treat as a WithVar binding if any var binding is within a with
                    node.bindings
                        .get_mut(name)
                        .unwrap()
                        .set_vm_location(VMLocation::WithVar)
                }

                // A `var arguments` in a function body scope (due to parameter expressions) must be
                // initialized from the arguments object, so the parent function scope needs an
                // implicit arguments object.
                if is_body_var_arguments {
                    let func_scope_id = node.parent.unwrap();
                    if self.needs_implicit_arguments_object(func_scope_id) {
                        self.add_implicit_arguments_object(func_scope_id);
                    }
                }

                let node_id_to_return = node_id_override.unwrap_or(node_id);

                return Ok(self.get_ast_node_ptr(node_id_to_return));
            } else {
                // Add var name to all scopes except for hoist target, so that later lexical
                // declarations can check for name conflicts.
                //
                // The only exception is that Annex B allows `var` declarations to conflict with
                // catch parameters. We do not add the var name to the catch scope in this case to
                // avoid throwing a redeclaration error later.
                if !(in_annex_b_mode
                    && matches!(node.kind, ScopeNodeKind::CatchBody)
                    && matches!(kind, BindingKind::Var))
                {
                    node.extra_var_names.insert(name);
                }
            }

            node_id = node.parent.unwrap();
        }
    }

    /// Add a binding to the current scope in the AST scope tree.
    pub fn add_binding_to_current_node(&mut self, name: AstStr<'a>, kind: BindingKind<'a>) {
        let name = self.own_name(name);
        self.get_ast_node_mut(self.current_node_id)
            .add_binding(name, kind, /* vm_location */ None, /* needs_tdz_check */ false);
    }

    /// Duplicate parameter names all share a single binding, which refers to the last parameter
    /// with that name. This means later duplicate parameters must overwrite earlier ones.
    fn can_overwrite_function_binding(
        existing_binding: &Option<&Binding>,
        new_binding_kind: &BindingKind,
    ) -> bool {
        let BindingKind::FunctionParameter { index: new_param_index, .. } = new_binding_kind else {
            return false;
        };

        let Some(existing_binding) = existing_binding else {
            return false;
        };

        let BindingKind::FunctionParameter { index: old_param_index, .. } = existing_binding.kind()
        else {
            return false;
        };

        new_param_index > old_param_index
    }

    /// Resolve a use of the given name in the provided AST scope id to the scope where the binding
    /// with that name is defined. If the name could not be statically resolved to a definition then
    /// return None.
    ///
    /// Marks the binding as captured if it is used by a nested function. Returns the resolved scope
    /// and whether the use is a capture.
    pub fn resolve_use<'b>(
        &mut self,
        use_scope_id: ScopeNodeId,
        name: AstStr<'b>,
        name_loc: Loc,
    ) -> (TaggedResolvedScope<'a>, bool) {
        let mut scope_id = use_scope_id;
        loop {
            let scope = self.get_ast_node(scope_id);
            let scope_parent = scope.parent;
            let scope_kind = scope.kind;
            let is_sloppy_eval_scope =
                matches!(scope_kind, ScopeNodeKind::Eval { is_strict: false, .. });
            let scope_supports_dynamic_bindings = scope.supports_dynamic_bindings;

            let mut found_def = scope.bindings.contains_key(name);

            // Arguments object bindings and new.target bindings are lazily added when we determine
            // that one is needed. We know that one is needed if we are looking up e.g. "arguments"
            // and it has not been resolved to a binding by the time we reach the function scope.
            if !found_def && name == "arguments" && self.needs_implicit_arguments_object(scope_id) {
                self.add_implicit_arguments_object(scope_id);
                found_def = true;
            }

            if !found_def
                && name == NEW_TARGET_BINDING_NAME.as_str()
                && self.needs_implicit_new_target(scope_id)
            {
                self.add_implicit_new_target(scope_id);
                found_def = true;
            }

            if found_def {
                let mut is_capture = false;

                // If the use is in a different function than the definition, mark the
                // binding as captured.
                let binding = if self.lookup_owner_function(scope_id)
                    != self.lookup_owner_function(use_scope_id)
                {
                    let bindings = &mut self.get_ast_node_mut(scope_id).bindings;
                    let binding = bindings.get_mut(name).unwrap();
                    binding.is_captured = true;
                    is_capture = true;

                    // Captured function parameters need a TDZ check if the closure appears (and
                    // therefore may be called) before the parameter is initialized.
                    if let BindingKind::FunctionParameter { init_pos, .. } = binding.kind() {
                        if name_loc.start < init_pos.get() {
                            binding.set_needs_tdz_check(true);
                        }
                    }

                    binding
                } else {
                    let bindings = &mut self.get_ast_node_mut(scope_id).bindings;
                    let binding = bindings.get_mut(name).unwrap();

                    // For const and let bindings in the same scope, check if the use is before the
                    // end of the binding's initialization. If so we need to check for the TDZ.
                    if let BindingKind::Const { init_pos }
                    | BindingKind::Let { init_pos }
                    | BindingKind::Class { init_pos, in_body_scope: false }
                    | BindingKind::CatchParameter { init_pos }
                    | BindingKind::FunctionParameter { init_pos, .. } = binding.kind()
                    {
                        if name_loc.start < init_pos.get() {
                            binding.set_needs_tdz_check(true);
                        }
                    }

                    // Home object bindings, class names in the scope body, and fields initializers
                    // must always be stored in a VM scope. Do not need to set this flag in other
                    // cases since binding will have been captured.
                    if matches!(
                        binding.kind(),
                        BindingKind::Class { in_body_scope: true, .. } | BindingKind::HomeObject
                    ) {
                        binding.force_vm_scope = true;
                    }

                    binding
                };

                // Var bindings in sloppy direct evals are added to the parent var scope, which
                // requires a dynamic lookup at runtime.
                if is_sloppy_eval_scope
                    && !binding.kind().is_lexically_scoped()
                    && !binding.kind().is_implicit_this()
                {
                    return (TaggedResolvedScope::unresolved_dynamic(), false);
                }

                let resolved_scope = TaggedResolvedScope::resolved(self.get_ast_node_ptr(scope_id));
                return (resolved_scope, is_capture);
            }

            // If we have not yet found the binding when exiting a scope that may contain dynamic
            // bindings, then the use is not statically resolvable.
            //
            // Note that `this` bindings can still be searched for past dynamic scopes.
            if scope_supports_dynamic_bindings && name != "this" {
                return (TaggedResolvedScope::unresolved_dynamic(), false);
            }

            // If we have not yet found the binding when exiting an eval scope, then the use is not
            // statically resolvable. Direct evals look up in their environment, while indirect
            // evals are known to be in the global scope so they can look up globally.
            if let ScopeNodeKind::Eval { is_direct, .. } = scope_kind {
                if is_direct {
                    return (TaggedResolvedScope::unresolved_dynamic(), false);
                } else {
                    return (TaggedResolvedScope::unresolved_global(), false);
                }
            }

            if let Some(parent_id) = scope_parent {
                scope_id = parent_id;
            } else {
                return (TaggedResolvedScope::unresolved_global(), false);
            }
        }
    }

    /// Look up the id of the function that owns an AST scope node. If the owner is a global,
    /// module, or eval scope then return None.
    fn lookup_owner_function(&self, scope_id: ScopeNodeId) -> Option<FunctionId> {
        let node = self.get_ast_node(scope_id);
        let owner_scope = self.get_ast_node(node.owner_scope);

        if let ScopeNodeKind::Function { id: func_id, .. } = owner_scope.kind {
            Some(func_id)
        } else {
            None
        }
    }

    /// Mark all scopes at the provided AST scope and above as being dynamically accessed, e.g. due
    /// to the presence of `eval` or `with`.
    pub fn support_dynamic_access_in_visible_bindings(&mut self, scope_id: ScopeNodeId) {
        let mut scope_id = scope_id;
        loop {
            let scope = self.get_ast_node_mut(scope_id);
            let scope_parent = scope.parent;

            if !scope.supports_dynamic_access {
                scope.supports_dynamic_access = true;

                // Any function scope that may be dynamically accessed must have an arguments object
                // and new.target since any binding could be looked up.
                //
                // IMPROVEMENT: new.target does not need to be added above `with` scopes
                if self.needs_implicit_arguments_object(scope_id) {
                    self.add_implicit_arguments_object(scope_id);
                }

                if self.needs_implicit_new_target(scope_id) {
                    self.add_implicit_new_target(scope_id);
                }
            }

            if let Some(parent_id) = scope_parent {
                scope_id = parent_id;
            } else {
                return;
            }
        }
    }

    /// Handle the presence of a sloppy direct eval in the current scope. This means the var hoist
    /// target of the current scope must support dynamic bindings, since the sloppy direct eval
    /// could introduce var bindings into that hoist target scope.
    pub fn mark_sloppy_direct_eval(&mut self) {
        let mut scope_id = self.current_node_id;
        loop {
            let scope = self.get_ast_node_mut(scope_id);

            match scope.kind {
                // No need to mark the global scope as supporting dynamic bindings
                ScopeNodeKind::Global
                // Module scopes do not need to support dynamic bindings since sloppy direct eval
                // cannot appear in modules.
                | ScopeNodeKind::Module => return,
                // Functions must be marked to support dynamic bindings
                ScopeNodeKind::Function { .. } | ScopeNodeKind::FunctionBody => {
                    scope.supports_dynamic_bindings = true;
                    return;
                }
                // Only strict eval needs to support dynamic bindings. In sloppy eval bindings are
                // added to the parent var scope.
                ScopeNodeKind::Eval { is_strict, .. } => {
                    if is_strict {
                        scope.supports_dynamic_bindings = true;
                    }

                    return;
                }
                _ => {
                    if let Some(parent_id) = scope.parent {
                        scope_id = parent_id;
                    } else {
                        panic!("could not find hoist target to mark as dynamic");
                    }
                }
            }
        }
    }

    fn needs_implicit_arguments_object(&mut self, scope_id: ScopeNodeId) -> bool {
        let scope = self.get_ast_node(scope_id);
        matches!(scope.kind, ScopeNodeKind::Function { is_arrow: false, .. })
            && !scope.bindings.contains_key(ARGUMENTS_NAME.as_str())
    }

    fn add_implicit_arguments_object(&mut self, scope_id: ScopeNodeId) {
        self.get_ast_node_mut(scope_id).add_binding(
            &ARGUMENTS_NAME,
            BindingKind::ImplicitArguments,
            None,
            false,
        );
    }

    fn needs_implicit_new_target(&mut self, scope_id: ScopeNodeId) -> bool {
        let scope = self.get_ast_node(scope_id);
        matches!(scope.kind, ScopeNodeKind::Function { is_arrow: false, .. })
            && !scope
                .bindings
                .contains_key(NEW_TARGET_BINDING_NAME.as_str())
    }

    fn add_implicit_new_target(&mut self, scope_id: ScopeNodeId) {
        self.get_ast_node_mut(scope_id).add_binding(
            &NEW_TARGET_BINDING_NAME,
            BindingKind::ImplicitNewTarget,
            None,
            false,
        );
    }

    /// Create and enter scopes for a named function expression. This includes both the function
    /// expression name scope and the function scope it wraps.
    pub fn enter_named_function_expression_scopes(
        &mut self,
        function_id: FunctionId,
        name: AstStr<'a>,
        binding_kind: BindingKind<'a>,
    ) -> (AstPtr<AstScopeNode<'a>>, AstPtr<AstScopeNode<'a>>) {
        let name_scope = self.enter_scope(ScopeNodeKind::FunctionExpressionName);
        self.add_binding_to_current_node(name, binding_kind);

        let func_scope =
            self.enter_scope(ScopeNodeKind::Function { id: function_id, is_arrow: false });

        // Despite the name scope wrapping the function scope, the name scope is owned by the
        // function scope so that its local binding is allocated in the function.
        self.get_ast_node_mut(name_scope.as_ref().id()).owner_scope = func_scope.as_ref().id();

        (name_scope, func_scope)
    }

    /// Whether the Annex B features are enabled.
    pub fn is_annex_b(&self) -> bool {
        self.options.annex_b
    }

    /// Set while the parser adds the name of a FunctionDeclaration of sloppy code that is neither
    /// async nor a generator and is lexically scoped, which may then redeclare another such
    /// FunctionDeclaration of the same statement list.
    pub fn set_annex_b_function_redeclaration(&mut self, value: bool) {
        self.annex_b_function_redeclaration = value;
    }

    /// Record a FunctionDeclaration of sloppy code that is neither async nor a generator and whose
    /// name was just added to the current scope, a Block or Switch scope, for the Block-Level
    /// Function Declarations Web Legacy Compatibility Semantics (B.3.2). Whether they apply is
    /// decided when its var hoist target is finished, as it depends on declarations that follow.
    /// A labelled declaration is recorded as not `eligible`: it is not directly contained in the
    /// statement list, but it still declares the name there.
    pub fn record_annex_b_block_function(&mut self, name: AstStr<'a>, eligible: bool) {
        let name = self.own_name(name);
        let decl_scope = self.current_node_id;
        let hoist_target = self.hoist_target_of(decl_scope);
        self.annex_b_pending.push(AnnexBPendingFunction {
            name,
            decl_scope,
            hoist_target,
            eligible,
        });
    }

    /// The var hoist target of the given scope: the nearest function, function body, script, eval,
    /// module or static initializer scope at or above it.
    pub fn hoist_target_of(&self, scope_id: ScopeNodeId) -> ScopeNodeId {
        let mut node_id = scope_id;
        loop {
            let node = self.get_ast_node(node_id);
            if node.kind.is_hoist_target() {
                return node_id;
            }

            node_id = node.parent.unwrap();
        }
    }

    /// Decide for each recorded block-level FunctionDeclaration of a finished var hoist target
    /// whether B.3.2 applies, and record the ones it applies to in the hoist target's
    /// `annex_b_functions`. For function code this also creates the var binding, so that uses of
    /// the name outside the block resolve to it.
    ///
    /// The steps are those of FunctionDeclarationInstantiation step 32.a.i (10.2.11),
    /// GlobalDeclarationInstantiation step 12.b.ii (16.1.7) and EvalDeclarationInstantiation step
    /// 13.b (19.2.1.4) that can be decided from the source text. The rest of the global and eval
    /// steps depends on the environment at run time (see `global_declaration_instantiation` and
    /// `eval_declaration_instantiation`).
    fn apply_annex_b_block_functions(&mut self, hoist_target: ScopeNodeId) {
        // Declarations in nested hoist targets were applied when those were exited, so the ones of
        // this hoist target are at the end of the pending list.
        let mut start = self.annex_b_pending.len();
        while start > 0 && self.annex_b_pending[start - 1].hoist_target == hoist_target {
            start -= 1;
        }

        if start == self.annex_b_pending.len() {
            return;
        }

        let decls = self.annex_b_pending.split_off(start);
        let hoist_kind = self.get_ast_node(hoist_target).kind;

        // 32.a.i / 12.b.ii / 13.b: for each FunctionDeclaration directly contained in the
        // StatementList of a Block, CaseClause or DefaultClause
        for (index, decl) in decls.iter().enumerate() {
            if !decl.eligible {
                continue;
            }

            let name = decl.name;

            // 32.a.i.2 / 12.b.ii.2 / 13.b.ii: if replacing the FunctionDeclaration with a
            // VariableStatement that has funcName as a BindingIdentifier would not produce any
            // Early Errors
            if !self.replacing_with_var_would_not_produce_early_errors(&decls, index) {
                continue;
            }

            let creates_var = match hoist_kind {
                ScopeNodeKind::Function { .. } | ScopeNodeKind::FunctionBody => {
                    // 32.a.i.2: ... and paramNames does not contain funcName
                    if self.is_function_parameter_name(hoist_target, name) {
                        continue;
                    }

                    // 32.a.i.2.b: "and funcName is not "arguments"". test262 also expects no copy
                    // to an `arguments` binding (block-decl-func-skip-arguments.js reads
                    // paramNames as including "arguments" when an arguments object is needed),
                    // and an arrow function's `arguments` is the enclosing one, so the name is
                    // skipped as a whole.
                    if name == ARGUMENTS_NAME.as_str() {
                        continue;
                    }

                    // 32.a.i.2.b: if instantiatedVariableNames does not contain funcName, create
                    // the var binding, initialized to undefined. Any binding of the name in the
                    // hoist target is now a var, a var scoped function or a parameter (lexical
                    // ones are early errors above).
                    let node = self.get_ast_node_mut(hoist_target);
                    if node.bindings.contains_key(name) {
                        false
                    } else {
                        node.add_binding(
                            name,
                            BindingKind::Var,
                            None,
                            /* needs_tdz_check */ false,
                        );
                        true
                    }
                }
                ScopeNodeKind::Global | ScopeNodeKind::Eval { .. } => {
                    // 12.b.ii.2.a.ii.2 / 13.b.ii.7.a: the binding is created at run time if
                    // declaredFuncOrVariableNames does not contain funcName (it then does for the
                    // following declarations of the name).
                    let node = self.get_ast_node(hoist_target);
                    let is_declared =
                        node.bindings.get(name).is_some_and(|binding| {
                            matches!(
                                binding.kind,
                                BindingKind::Var | BindingKind::Function { is_lexical: false, .. }
                            )
                        }) || node.annex_b_functions.iter().any(|func| func.name == name);

                    !is_declared
                }
                // Module and class static initializer code is strict
                _ => continue,
            };

            let decl_scope = decl.decl_scope;
            self.get_ast_node_mut(hoist_target)
                .annex_b_functions
                .push(AnnexBFunction { name, decl_scope, creates_var, enabled: Cell::new(true) });
        }
    }

    /// The early error check of FunctionDeclarationInstantiation step 32.a.i.2,
    /// GlobalDeclarationInstantiation step 12.b.ii.2 and EvalDeclarationInstantiation step
    /// 13.b.ii: whether replacing the FunctionDeclaration `decls[index]` with a VariableStatement
    /// that has its name as a BindingIdentifier would not produce any Early Errors for the code of
    /// its hoist target. `decls` are all recorded declarations of that hoist target.
    fn replacing_with_var_would_not_produce_early_errors(
        &self,
        decls: &[AnnexBPendingFunction<'a>],
        index: usize,
    ) -> bool {
        let decl = &decls[index];

        // The var would be in the VarDeclaredNames of the statement list that contains the
        // declaration, whose LexicallyDeclaredNames keep any other FunctionDeclaration of the name
        // (allowed duplicates in sloppy code): 14.2.1 / 14.12.1, "It is a Syntax Error if any
        // element of the LexicallyDeclaredNames of StatementList also occurs in the
        // VarDeclaredNames of StatementList".
        let has_other_declaration = decls.iter().enumerate().any(|(other_index, other)| {
            other_index != index && other.decl_scope == decl.decl_scope && other.name == decl.name
        });
        if has_other_declaration {
            return false;
        }

        // The same holds for each enclosing statement list up to and including the function,
        // script or eval code (its top level lexical declarations are in the hoist target).
        let mut node_id = decl.decl_scope;
        while node_id != decl.hoist_target {
            node_id = self.get_ast_node(node_id).parent.unwrap();
            let node = self.get_ast_node(node_id);

            if let Some(binding) = node.bindings.get(decl.name) {
                if binding.kind.is_lexically_scoped() {
                    // B.3.4 (changes to 14.15.1): the Block of a Catch may declare a var with the
                    // name of a CatchParameter that is a BindingIdentifier.
                    let is_simple_catch_parameter = binding.kind.is_catch_parameter()
                        && node.catch_parameter_is_binding_identifier;
                    if !is_simple_catch_parameter {
                        return false;
                    }
                }
            }
        }

        true
    }

    /// Whether the name is one of the parameter names (paramNames in 10.2.11) of the function whose
    /// var hoist target is given.
    fn is_function_parameter_name(&self, hoist_target: ScopeNodeId, name: AstStr) -> bool {
        let node = self.get_ast_node(hoist_target);

        // With parameter expressions the parameters are in the function scope around the body
        let func_node = if node.kind == ScopeNodeKind::FunctionBody {
            self.get_ast_node(node.parent.unwrap())
        } else {
            node
        };

        // A function declaration that overwrote a parameter keeps the parameter's index
        func_node
            .bindings
            .get(name)
            .is_some_and(|binding| binding.kind.parameter_index().is_some())
    }
}

/// Functions for constructing the VM scope tree.
impl ScopeTree<'_> {
    /// Complete an AST scope node and create a corresponding VM scope node if necessary.
    ///
    /// Optionally provide the number of arguments for a mapped arguments object if one needs to be
    /// created.
    pub fn finish_vm_scope_node(
        &mut self,
        ast_node_id: ScopeNodeId,
        num_extra_slots: Option<usize>,
    ) {
        // Extract allocator
        let alloc = self.alloc;

        let vm_node_id = self.vm_nodes.len();
        let ast_node = self.get_ast_node_mut(ast_node_id);
        let owner_scope = ast_node.owner_scope;

        let mut bindings = AstSliceBuilder::new(alloc::vec![in alloc]);

        let has_mapped_arguments_object =
            num_extra_slots.is_some() && matches!(ast_node.kind(), ScopeNodeKind::Function { .. });

        if ast_node.kind() == ScopeNodeKind::Global {
            // The first slot in a global scope always contains the realm
            bindings.push(REALM_SCOPE_SLOT_NAME.as_str());
        } else if ast_node.kind() == ScopeNodeKind::Module {
            // The first slot in a module scope always contains the module object
            bindings.push(MODULE_SCOPE_SLOT_NAME.as_str());
        } else if has_mapped_arguments_object {
            // A mapped arguments object requires that all arguments be placed in the VM scope node
            // in order.

            // Bindings start out with unresolvable "%private" for each argument name, which will be
            // overridden by the actual binding name for accessible arguments. Some arguments cannot
            // be accessed by name (since they are shadowed by another binding) and will remain
            // unresolvable.
            for _ in 0..num_extra_slots.unwrap() {
                bindings.push(SHADOWED_SCOPE_SLOT_NAME.as_str());
            }

            for (name, binding) in ast_node.bindings.iter_mut() {
                let arg_index = if let Some(index) = binding.kind().parameter_index() {
                    index as usize
                } else {
                    continue;
                };

                // Do not overwrite a WithVar, which will be dynamically accessed
                if !matches!(binding.vm_location(), Some(VMLocation::WithVar)) {
                    binding.set_vm_location(VMLocation::Scope {
                        scope_id: vm_node_id,
                        index: arg_index,
                    });
                }

                bindings[arg_index] = *name;
            }
        } else if num_extra_slots.is_some() && ast_node.kind() == ScopeNodeKind::Class {
            // Extra slots for a class scope will hold computed and private field names
            for _ in 0..num_extra_slots.unwrap() {
                bindings.push(CLASS_FIELD_SLOT_NAME.as_str());
            }
        }

        // Collect all bindings that must appear in a VM scope node
        for (name, binding) in ast_node.bindings.iter_mut() {
            // Function parameters for mapped arguments object have already been placed in the scope
            if has_mapped_arguments_object && binding.kind().parameter_index().is_some() {
                continue;
            }

            if binding.is_module_binding() {
                // Imported or exported bindings must be placed in the module's VM scope
                binding.set_vm_location(VMLocation::ModuleScope {
                    scope_id: vm_node_id,
                    index: bindings.len(),
                });
                bindings.push(*name);
                continue;
            } else if binding.kind().is_namespace_import() {
                // Namespace import bindings are always placed in the module's scope, but as regular
                // scope values instead of BoxedValues.
                binding.set_vm_location(VMLocation::Scope {
                    scope_id: vm_node_id,
                    index: bindings.len(),
                });
                bindings.push(*name);
                continue;
            }

            let is_global = matches!(binding.vm_location, Some(VMLocation::Global));
            let is_eval_var = matches!(binding.vm_location, Some(VMLocation::EvalVar));
            let is_with_var = matches!(binding.vm_location, Some(VMLocation::WithVar));

            let needs_vm_scope = (
                // All captured bindings must be placed in a VM scope
                binding.is_captured ||
                binding.force_vm_scope ||
                // All private names must be placed in a VM scope
                matches!(binding.kind(), BindingKind::PrivateName) ||
                // We sometimes force bindings in VM scopes due to dynamic accesses.
                // e.g. in the presence of `eval` or `with`
                //
                // No need to place the global `this` in a VM scope unless it is captured.
                (ast_node.supports_dynamic_access &&
                    !(*name == "this" &&
                        (ast_node.kind == ScopeNodeKind::Global
                            || ast_node.kind == ScopeNodeKind::Module)))
            ) && (
                // Eval var bindings will already be placed in VM scope objects during eval
                // declaration instantiation.
                !is_eval_var
            ) && (
                // With var bindings will be placed in VM scope objects due to dynamic loads and
                // stores.
                !is_with_var
            );

            if needs_vm_scope {
                // Global bindings are always accessible so no need to place in VM scope. The only
                // exception is the global `this` which must be accessed through a VM scope.
                if !is_global || binding.is_implicit_this() {
                    // Mark the VM location for the binding
                    binding.set_vm_location(VMLocation::Scope {
                        scope_id: vm_node_id,
                        index: bindings.len(),
                    });

                    bindings.push(*name);
                }

                // Lexical bindings in VM scope nodes need a TDZ check as we do not analyze whether
                // they are guaranteed to be initialized before use.
                //
                // Function parameters are guaranteed to be initialized before any use after
                // parameter initialization. But if they may be dynamically accessed during
                // parameter initialization they need a TDZ check as well.
                //
                // Function expression names are initialized before any code in the function runs,
                // so they never need a TDZ check.
                if (!binding.kind().is_function_parameter()
                    || ast_node.may_dynamically_access_uninitialized_parameters)
                    && !binding.kind().is_function_expression_name()
                {
                    binding.set_needs_tdz_check(true);
                }
            }

            // With vars still need to be added to the VM scope (except in the global scope, where
            // it will be added to the global object).
            if is_with_var && ast_node.kind != ScopeNodeKind::Global {
                bindings.push(*name);
            }

            // All globals need a TDZ check as they could be captured by any function
            if is_global {
                binding.set_needs_tdz_check(true);
            }
        }

        // Only if there are bindings to place in the scope do we need to create a VM scope node.
        // Any scopes that could contain dynamic bindings must also have a VM scope node to hold the
        // dynamic object. This includes all with scopes.
        if !bindings.is_empty()
            || ast_node.supports_dynamic_bindings
            || ast_node.allow_empty_vm_node
        {
            self.vm_nodes
                .push(VMScopeNode { bindings: bindings.build() });
            self.get_ast_node_mut(ast_node_id).vm_scope = Some(vm_node_id)
        }

        // Maintain a list of owned AST scope nodes for each global, module, function, or eval
        // scope (in reverse order) so that locals can be calculated later.
        let owner_scope = self.get_ast_node_mut(owner_scope);
        owner_scope.owned_scopes.push(ast_node_id);
    }

    pub fn finish_vm_scope_tree(&mut self) {
        self.finish_vm_scope_tree_from(0);
    }

    /// Place the locals of the owner scopes with id `first_node` or higher, e.g. the nodes added
    /// when one function was parsed again on its own (earlier nodes are finished already).
    pub fn finish_vm_scope_tree_from(&mut self, first_node: ScopeNodeId) {
        for i in first_node..self.ast_nodes.len() {
            if self.ast_nodes[i]
                .as_ref()
                .is_none_or(|node| node.owned_scopes.is_empty())
            {
                continue;
            }

            let mut num_local_registers = 0;

            // Extract owned scopes
            let mut owned_scopes = alloc::vec![in self.alloc];
            std::mem::swap(&mut owned_scopes, &mut self.get_ast_node_mut(i).owned_scopes);

            // All bindings without a location are placed in local registers in the owner scope.
            //  - Except for implicit this, which is either captured or loaded from the this slot.
            //  - Except for class name binding in body scope, which is always stored in scope
            for owned_scope in owned_scopes.iter().rev() {
                let owned_scope = self.get_ast_node_mut(*owned_scope);
                for (_, binding) in owned_scope.bindings.iter_mut() {
                    if binding.vm_location.is_none()
                        && !matches!(
                            binding.kind(),
                            BindingKind::ImplicitThis { .. }
                                | BindingKind::HomeObject
                                | BindingKind::Class { in_body_scope: true, .. }
                        )
                    {
                        binding.set_vm_location(VMLocation::LocalRegister(num_local_registers));
                        num_local_registers += 1;
                    }
                }
            }

            self.get_ast_node_mut(i).num_local_registers = num_local_registers;
        }
    }
}

pub type ScopeNodeId = usize;

#[derive(Clone, Copy, PartialEq)]
pub enum ScopeNodeKind {
    Global,
    Module,
    Function {
        /// A unique identifier for the function. Currently the function's starting source position.
        id: FunctionId,
        /// Whether this is an arrow function
        is_arrow: bool,
    },
    /// A function body scope only appears when the function has parameter expressions. In this case
    /// the function body is the hoist target for all declarations in the body, keeping them
    /// separate from the parameters.
    FunctionBody,
    /// Scope holding only the name of a named function expression. Wraps the function scope.
    FunctionExpressionName,
    Block,
    Switch,
    /// Body of a class declaration.
    Class,
    /// Block body of a catch clause.
    CatchBody,
    With,
    /// Scope created within an eval. Var bindings of a sloppy direct eval are added to the parent
    /// var scope.
    Eval {
        /// Whether this is a direct or indirect eval.
        is_direct: bool,
        /// Whether the eval body is in strict mode.
        is_strict: bool,
    },
    /// Scope for a static initializer block of a class. Acts as a var hoist target.
    StaticInitializer,
}

impl ScopeNodeKind {
    fn is_hoist_target(&self) -> bool {
        match self {
            ScopeNodeKind::Global
            | ScopeNodeKind::Module
            | ScopeNodeKind::Function { .. }
            | ScopeNodeKind::FunctionBody
            // Sloppy eval will add var bindings to the parent var scope. However we still consider
            // the eval scope to be a hoist target during analysis.
            | ScopeNodeKind::Eval { .. }
            | ScopeNodeKind::StaticInitializer => true,
            | ScopeNodeKind::Block
            | ScopeNodeKind::Switch
            | ScopeNodeKind::Class
            | ScopeNodeKind::CatchBody
            | ScopeNodeKind::FunctionExpressionName
            | ScopeNodeKind::With => false,
        }
    }
}

pub struct AstScopeNode<'a> {
    id: ScopeNodeId,
    parent: Option<ScopeNodeId>,
    kind: ScopeNodeKind,
    /// Bindings declared in this scope. Each name maps to the last binding with that name declared
    /// in the scope. Earlier var bindings may be overwritten by later var bindings, which is fine
    /// since they reference the same binding (though may contain different BindingKinds).
    bindings: AstIndexMap<'a, AstStr<'a>, Binding<'a>>,
    /// All var declared names that are in scope at this node but not included in the bindings map.
    /// - Includes the set of all var declared names in child scopes
    /// - For function body scopes, includes the parameter names
    extra_var_names: AstHashSet<'a, AstStr<'a>>,
    /// Total number of bindings added to map, including duplicates.
    num_bindings: usize,
    /// Whether at least one duplicate binding was added to map, replacing an earlier binding.
    has_duplicates: bool,
    /// Whether this scope may be dynamically accessed (meaning there may be runtime lookups by
    /// name, e.g. due to the presence of `eval` or `with`). This means all bindings in this scope
    /// must be placed in a VM scope node so they can be accessed at runtime.
    supports_dynamic_access: bool,
    /// Whether this scope may have bindings dynamically added to it at runtime, preventing static
    /// analysis e.g. due to the presence of `eval` or `with`.
    supports_dynamic_bindings: bool,
    /// Whether to create a VM node when this AST scope node has no bindings.
    allow_empty_vm_node: bool,
    /// Whether this is a function scope node whose parameters may be dynamically accessed (e.g.
    /// by an eval or with) during function parameter initialization, before they are initialized.
    may_dynamically_access_uninitialized_parameters: bool,
    /// The global, module, function, or eval AST scope node that this scope's locals will be placed
    /// in. For those nodes this points to itself.
    ///
    /// This is normally the nearest ancestor of the aforementioned type. The one exception is a
    /// function expression name scope, which is owned by the function scope it wraps.
    owner_scope: ScopeNodeId,
    /// The set of all AST scope nodes directly owned by this scope, as described above.
    owned_scopes: AstVec<'a, ScopeNodeId>,
    /// If this is a global, module, function, or eval AST scope node, the total number of local
    /// registers needed for the locals in all owned scopes.
    num_local_registers: usize,
    /// Concrete VM scope node id for this AST scope node, if any bindings of this AST scope node
    /// need to be located in a VM scope node.
    vm_scope: Option<ScopeNodeId>,
    /// For a function scope node, what is needed to parse the function again on its own after the
    /// AST has been dropped (see `parser::reparse`).
    reparse: Option<AstBox<'a, ReparseInfo>>,
    /// On a var hoist target: the block-level FunctionDeclarations of its code that the Block-Level
    /// Function Declarations Web Legacy Compatibility Semantics (B.3.2) apply to, in source order.
    /// Decided when the hoist target is finished by the parser, before the analysis resolves uses.
    annex_b_functions: AstVec<'a, AnnexBFunction<'a>>,
    /// On the scope of a catch parameter: whether the parameter is a BindingIdentifier, whose name
    /// a var in the catch block may share (B.3.4).
    catch_parameter_is_binding_identifier: bool,
}

/// A block-level FunctionDeclaration whose evaluation also sets the var binding of its name in the
/// enclosing function, script or eval code (B.3.2).
pub struct AnnexBFunction<'a> {
    /// The declaration's name, in the scope tree's arena.
    name: AstStr<'a>,
    /// The Block or Switch scope whose statement list contains the declaration.
    decl_scope: ScopeNodeId,
    /// Whether the var binding is created for this declaration, as no var, var scoped function
    /// or earlier declaration of this list has the name. In function code the binding is in the
    /// hoist target; in script and eval code it is created at run time.
    creates_var: bool,
    /// Cleared by EvalDeclarationInstantiation when the run time conditions of step 13.b.ii do not
    /// hold, so that evaluating the declaration does not set the var binding.
    enabled: Cell<bool>,
}

impl<'a> AnnexBFunction<'a> {
    pub fn name(&self) -> AstStr<'a> {
        self.name
    }

    pub fn decl_scope(&self) -> ScopeNodeId {
        self.decl_scope
    }

    pub fn creates_var(&self) -> bool {
        self.creates_var
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.get()
    }

    pub fn disable(&self) {
        self.enabled.set(false);
    }
}

const INITIAL_SCOPE_ID: ScopeNodeId = 0;

impl<'a> AstScopeNode<'a> {
    pub fn id(&self) -> ScopeNodeId {
        self.id
    }

    pub fn parent(&self) -> Option<ScopeNodeId> {
        self.parent
    }

    pub fn owner_scope(&self) -> ScopeNodeId {
        self.owner_scope
    }

    /// A copy of this node in `alloc`, names copied with `copy_name`.
    fn copy_into<'b>(
        &self,
        alloc: AstAlloc<'b>,
        copy_name: &impl Fn(&Wtf8Str) -> AstStr<'b>,
    ) -> AstScopeNode<'b> {
        let mut bindings = AstIndexMap::new_in(alloc);
        bindings.reserve(self.bindings.len());
        for (name, binding) in self.bindings.iter() {
            // SAFETY: a binding holds no data it owns (cells of plain values, a VM location and a
            // kind whose only pointer, `BindingKind::Function::func_node`, points into the AST
            // and is not followed once the AST is gone), so a bitwise copy with another lifetime is
            // a valid binding.
            let copy = unsafe { std::mem::transmute_copy::<Binding<'a>, Binding<'b>>(binding) };
            bindings.insert(copy_name(name), copy);
        }
        let mut extra_var_names = AstHashSet::new_in(alloc);
        for name in self.extra_var_names.iter() {
            extra_var_names.insert(copy_name(name));
        }
        let mut owned_scopes = alloc::vec![in alloc];
        owned_scopes.extend_from_slice(&self.owned_scopes);
        let mut annex_b_functions = alloc::vec![in alloc];
        for func in self.annex_b_functions.iter() {
            annex_b_functions.push(AnnexBFunction {
                name: copy_name(func.name),
                decl_scope: func.decl_scope,
                creates_var: func.creates_var,
                enabled: Cell::new(func.enabled.get()),
            });
        }
        AstScopeNode {
            id: self.id,
            parent: self.parent,
            kind: self.kind,
            bindings,
            extra_var_names,
            num_bindings: self.num_bindings,
            has_duplicates: self.has_duplicates,
            supports_dynamic_access: self.supports_dynamic_access,
            supports_dynamic_bindings: self.supports_dynamic_bindings,
            allow_empty_vm_node: self.allow_empty_vm_node,
            may_dynamically_access_uninitialized_parameters: self
                .may_dynamically_access_uninitialized_parameters,
            owner_scope: self.owner_scope,
            owned_scopes,
            num_local_registers: self.num_local_registers,
            vm_scope: self.vm_scope,
            reparse: self
                .reparse
                .as_deref()
                .map(|info| AstBox::new_in(*info, alloc)),
            annex_b_functions,
            catch_parameter_is_binding_identifier: self.catch_parameter_is_binding_identifier,
        }
    }

    /// The block-level FunctionDeclarations of this var hoist target's code that B.3.2 applies to.
    pub fn annex_b_functions(&self) -> &[AnnexBFunction<'a>] {
        &self.annex_b_functions
    }

    /// The B.3.2 entry of the FunctionDeclaration with the given name in the given Block or Switch
    /// scope, if B.3.2 applies to it and it was not disabled.
    pub fn enabled_annex_b_function(
        &self,
        name: &Wtf8Str,
        decl_scope: ScopeNodeId,
    ) -> Option<&AnnexBFunction<'a>> {
        self.annex_b_functions
            .iter()
            .find(|func| func.decl_scope == decl_scope && func.name == name && func.enabled.get())
    }

    pub fn set_catch_parameter_is_binding_identifier(&mut self, value: bool) {
        self.catch_parameter_is_binding_identifier = value;
    }

    /// Whether the name is bound in this scope by a lexically scoped FunctionDeclaration that is
    /// neither async nor a generator, and no var in a nested scope has the name.
    fn is_function_declaration_name(&self, name: AstStr) -> bool {
        if self.extra_var_names.contains(name) {
            return false;
        }

        match self.bindings.get(name).map(|binding| &binding.kind) {
            Some(BindingKind::Function {
                is_lexical: true,
                is_expression: false,
                func_node,
                ..
            }) => {
                let func = func_node.as_ref();
                !func.is_async() && !func.is_generator()
            }
            _ => false,
        }
    }

    /// What is needed to parse this node's function again on its own, if recorded.
    pub fn reparse_info(&self) -> Option<&ReparseInfo> {
        self.reparse.as_deref()
    }

    pub fn reparse_info_mut(&mut self) -> Option<&mut ReparseInfo> {
        self.reparse.as_deref_mut()
    }

    pub fn owned_scopes(&self) -> &[ScopeNodeId] {
        &self.owned_scopes
    }

    pub fn extra_var_names(&self) -> &AstHashSet<'a, AstStr<'a>> {
        &self.extra_var_names
    }

    pub fn num_bindings(&self) -> usize {
        self.num_bindings
    }

    pub fn has_duplicates(&self) -> bool {
        self.has_duplicates
    }

    pub fn allow_empty_vm_node(&self) -> bool {
        self.allow_empty_vm_node
    }

    pub fn may_dynamically_access_uninitialized_parameters(&self) -> bool {
        self.may_dynamically_access_uninitialized_parameters
    }

    pub fn kind(&self) -> ScopeNodeKind {
        self.kind
    }

    pub fn set_kind(&mut self, kind: ScopeNodeKind) {
        self.kind = kind;
    }

    pub fn num_local_registers(&self) -> usize {
        self.num_local_registers
    }

    pub fn vm_scope_id(&self) -> Option<ScopeNodeId> {
        self.vm_scope
    }

    pub fn supports_dynamic_bindings(&self) -> bool {
        self.supports_dynamic_bindings
    }

    pub fn set_allow_empty_vm_node(&mut self, value: bool) {
        self.allow_empty_vm_node = value;
    }

    pub fn supports_dynamic_access(&self) -> bool {
        self.supports_dynamic_access
    }

    pub fn set_may_dynamically_access_uninitialized_parameters(&mut self, value: bool) {
        self.may_dynamically_access_uninitialized_parameters = value;
    }

    fn add_binding(
        &mut self,
        name: AstStr<'a>,
        kind: BindingKind<'a>,
        vm_location: Option<VMLocation>,
        needs_tdz_check: bool,
    ) {
        let insert_result = self
            .bindings
            .insert(name, Binding::new(kind, self.num_bindings, vm_location, needs_tdz_check));

        if insert_result.is_some() {
            self.has_duplicates = true;
        }

        self.num_bindings += 1;
    }

    pub fn has_binding(&self, name: AstStr) -> bool {
        self.bindings.contains_key(name)
    }

    pub fn get_binding<'b>(&self, name: AstStr<'b>) -> &Binding<'a> {
        self.bindings.get(name).unwrap()
    }

    pub fn get_binding_opt<'b>(&self, name: AstStr<'b>) -> Option<&Binding<'a>> {
        self.bindings.get(name)
    }

    pub fn get_binding_mut<'b>(&mut self, name: AstStr<'b>) -> &mut Binding<'a> {
        self.bindings.get_mut(name).unwrap()
    }

    /// Iterator over all bindings in this scope, including function parameters and catch parameters.
    pub fn iter_bindings(&self) -> impl DoubleEndedIterator<Item = (&AstStr<'a>, &Binding<'a>)> {
        self.bindings.iter()
    }

    /// Iterator over all VarScopedDeclarations in this scope. Note that this does not include
    /// function parameters or implicit function bindings, which are var scoped but do not count as
    /// VarScopedDeclarations.
    pub fn iter_var_decls(&self) -> impl DoubleEndedIterator<Item = (&AstStr<'a>, &Binding<'a>)> {
        self.bindings.iter().filter(|(_, binding)| {
            matches!(
                binding.kind,
                BindingKind::Var | BindingKind::Function { is_lexical: false, .. }
            )
        })
    }

    /// Error if the provided lexical name is already declared in this scope.
    pub fn error_if_lexical_name_already_declared(&self, name: AstStr) -> Option<ParseError> {
        // Error if there is already any binding with this name in the current scope, which is
        // guaranteed to detect conflicting lexically scoped bindings and var scoped bindings
        // declared in this scope.
        if let Some(binding) = self.bindings.get(name) {
            return Some(ParseError::new_name_redeclaration(
                name.to_owned_in(Global),
                binding.kind.clone(),
            ));
        }

        // Then check for other conflicting var scoped bindings, e.g. in child scopes
        if self.extra_var_names.contains(name) {
            return Some(ParseError::new_name_redeclaration(
                name.to_owned_in(Global),
                // Guaranteed to conflict with a `var` binding, as the other var scoped names -
                // var scoped functions and function parameters - cannot appear in child scopes.
                BindingKind::Var,
            ));
        }

        None
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum BindingKind<'a> {
    Var,
    Const {
        /// The source position after which this constant has been initialized, inclusive.
        init_pos: Cell<Pos>,
    },
    Let {
        /// The source position after which this let has been initialized, inclusive.
        init_pos: Cell<Pos>,
    },
    Function {
        /// Whether this is a LexicallyScopedDeclaration or a VarScopedDeclaration from the spec.
        /// Only a VarScopedDeclaration if this is a toplevel function declaration within a
        /// script, function, or class static block.
        is_lexical: bool,
        /// Whether this is the name of a function expression.
        is_expression: bool,
        /// Function AST node.
        func_node: AstPtr<ast::Function<'a>>,
        /// If this function declaration overwrote a function parameter with the same name, the
        /// index of that parameter. Necessary for mapped arguments objects.
        parameter_index: Option<u32>,
    },
    FunctionParameter {
        /// The source position after which this parameter has been initialized, inclusive.
        init_pos: Cell<Pos>,
        /// Index of the parameter in the function's parameter list.
        index: u32,
    },
    Class {
        /// Whether this is the class name binding inside the class body, as opposed to in the
        /// surrounding scope.
        in_body_scope: bool,
        /// The source position after which this class has been initialized, inclusive. Only set
        /// for the class name binding in the parent scope, not the binding in the class body.
        init_pos: Cell<Pos>,
    },
    CatchParameter {
        /// The source position after which this parameter has been initialized, inclusive.
        init_pos: Cell<Pos>,
    },
    Import {
        /// Whether this is a namespace import.
        is_namespace: bool,
    },
    /// An implicit `this` binding introduced in a function or root scope.
    ImplicitThis {
        /// Whether this is the `this` value for a derived constructor.
        in_derived_constructor: bool,
    },
    /// An implicit `arguments` binding introduced in a function. Note that any var or var function
    /// is treated as an "explicit" `arguments` binding and will require an arguments object.
    ImplicitArguments,
    /// new.target is treated as a binding.
    ImplicitNewTarget,
    /// The constructor itself is treated as a binding in derived constructors, since super calls
    /// need to look up the constructor.
    DerivedConstructor,
    /// The home object for methods defined on an object literal or class.
    HomeObject,
    /// A private name introduced in a class. Includes the "#" prefix.
    PrivateName,
    /// A default exported expression
    DefaultExportExpression,
}

impl<'a> BindingKind<'a> {
    pub fn new_function_parameter(index: u32) -> BindingKind<'a> {
        BindingKind::FunctionParameter { init_pos: Cell::new(0), index }
    }

    pub fn new_implicit_this(in_derived_constructor: bool) -> BindingKind<'a> {
        BindingKind::ImplicitThis { in_derived_constructor }
    }

    /// Whether this is a LexicallyScopedDeclaration from the spec.
    pub fn is_lexically_scoped(&self) -> bool {
        match self {
            BindingKind::Var
            | BindingKind::FunctionParameter { .. }
            | BindingKind::ImplicitThis { .. }
            | BindingKind::ImplicitArguments
            | BindingKind::ImplicitNewTarget
            | BindingKind::DerivedConstructor
            | BindingKind::HomeObject
            | BindingKind::PrivateName => false,
            BindingKind::Function { is_lexical, .. } => *is_lexical,
            BindingKind::Const { .. }
            | BindingKind::Let { .. }
            | BindingKind::Class { .. }
            | BindingKind::CatchParameter { .. }
            | BindingKind::Import { .. }
            | BindingKind::DefaultExportExpression => true,
        }
    }

    pub fn is_immutable(&self) -> bool {
        matches!(
            self,
            BindingKind::Const { .. }
                | BindingKind::Class { in_body_scope: true, .. }
                | BindingKind::Import { .. }
        )
    }

    pub fn is_function(&self) -> bool {
        matches!(self, BindingKind::Function { .. })
    }

    pub fn is_function_expression_name(&self) -> bool {
        matches!(self, BindingKind::Function { is_expression: true, .. })
    }

    pub fn is_implicit_this(&self) -> bool {
        matches!(self, BindingKind::ImplicitThis { .. })
    }

    pub fn is_implicit_this_in_derived_constructor(&self) -> bool {
        matches!(self, BindingKind::ImplicitThis { in_derived_constructor: true })
    }

    pub fn is_implicit_arguments(&self) -> bool {
        matches!(self, BindingKind::ImplicitArguments)
    }

    pub fn is_function_parameter(&self) -> bool {
        matches!(self, BindingKind::FunctionParameter { .. })
    }

    /// If this binding occupies a function parameter's slot, the index of that parameter. Note that
    /// a function declaration takes over the slot of a parameter with the same name.
    pub fn parameter_index(&self) -> Option<u32> {
        match self {
            BindingKind::FunctionParameter { index, .. } => Some(*index),
            BindingKind::Function { parameter_index, .. } => *parameter_index,
            _ => None,
        }
    }

    pub fn is_catch_parameter(&self) -> bool {
        matches!(self, BindingKind::CatchParameter { .. })
    }

    pub fn is_private_name(&self) -> bool {
        matches!(self, BindingKind::PrivateName)
    }

    pub fn is_import(&self) -> bool {
        matches!(self, BindingKind::Import { .. })
    }

    pub fn is_namespace_import(&self) -> bool {
        matches!(self, BindingKind::Import { is_namespace: true })
    }

    fn has_tdz(&self) -> bool {
        matches!(
            self,
            BindingKind::Const { .. }
                | BindingKind::Let { .. }
                | BindingKind::Class { .. }
                | BindingKind::CatchParameter { .. }
                | BindingKind::FunctionParameter { .. }
                | BindingKind::DefaultExportExpression
        )
    }

    /// A binding with the name `arguments` requires that an arguments object be created if:
    /// - It is an implicit `arguments` binding (i.e. a use of `arguments` which reaches a function
    ///   when resolved to a binding).
    /// - It is an explicit `arguments` binding (i.e. a var or var function declaration with the
    ///   name `arguments`).
    pub fn is_valid_arguments_kind(&self) -> bool {
        matches!(
            self,
            BindingKind::Var
                | BindingKind::Function { is_lexical: false, .. }
                | BindingKind::ImplicitArguments
        )
    }
}

pub struct Binding<'a> {
    kind: BindingKind<'a>,
    /// Index of the binding in the scope's bindings map. Used to determine the order of duplicate
    /// bindings.
    index: usize,
    /// Whether this binding is captured by a nested function. Set during use analysis.
    is_captured: bool,
    /// Whether this binding is exported from its module.
    is_exported: Cell<bool>,
    /// Whether to force this binding to be in a VM scope. Set during use analysis.
    force_vm_scope: bool,
    /// If this is a const or let declaration, whether there is some use that requires a TDZ check.
    needs_tdz_check: Cell<bool>,
    /// Whether this binding is assigned to in a nested assignment (or update) expression anywhere
    /// in the function. Set during analysis.
    has_nested_assignment: Cell<bool>,
    /// Location of the binding in the VM, must be set before bytecode generation.
    vm_location: Option<VMLocation>,
}

impl<'a> Binding<'a> {
    fn new(
        kind: BindingKind<'a>,
        index: usize,
        vm_location: Option<VMLocation>,
        needs_tdz_check: bool,
    ) -> Binding<'a> {
        Binding {
            kind,
            index,
            is_captured: false,
            is_exported: Cell::new(false),
            force_vm_scope: false,
            needs_tdz_check: Cell::new(needs_tdz_check),
            has_nested_assignment: Cell::new(false),
            vm_location,
        }
    }

    pub fn kind(&self) -> &BindingKind<'a> {
        &self.kind
    }

    pub fn is_immutable(&self) -> bool {
        self.kind.is_immutable()
    }

    pub fn is_implicit_this(&self) -> bool {
        self.kind.is_implicit_this()
    }

    pub fn vm_location(&self) -> Option<VMLocation> {
        self.vm_location
    }

    /// Index of this binding in its scope's bindings map, in insertion order.
    pub fn binding_index(&self) -> usize {
        self.index
    }

    /// Whether this binding is captured by a nested function.
    pub fn is_captured(&self) -> bool {
        self.is_captured
    }

    /// Whether this binding is forced to be in a VM scope.
    pub fn force_vm_scope(&self) -> bool {
        self.force_vm_scope
    }

    pub fn set_vm_location(&mut self, location: VMLocation) {
        self.vm_location = Some(location);
    }

    /// Whether this binding needs TDZ checks when accessed.
    pub fn needs_tdz_check(&self) -> bool {
        self.kind().has_tdz() && self.needs_tdz_check.get()
    }

    fn set_needs_tdz_check(&self, value: bool) {
        self.needs_tdz_check.set(value);
    }

    pub fn is_exported(&self) -> bool {
        self.is_exported.get()
    }

    pub fn has_nested_assignment(&self) -> bool {
        self.has_nested_assignment.get()
    }

    pub fn set_has_nested_assignment(&self, value: bool) {
        self.has_nested_assignment.set(value);
    }

    pub fn set_is_exported(&self, value: bool) {
        self.is_exported.set(value);

        // If a binding has a TDZ and is exported then TDZ checks are necessary
        if value && self.kind().has_tdz() {
            self.set_needs_tdz_check(true);
        }
    }

    pub fn is_module_binding(&self) -> bool {
        // Note that `import * as foo` is not a module binding, namespace object is instead stored
        // in scope without needing a BoxedValue.
        matches!(self.kind(), BindingKind::Import { is_namespace: false }) || self.is_exported()
    }
}

#[derive(Clone, Copy, Debug)]
pub enum VMLocation {
    /// Var binding in the global scope.
    Global,
    /// Argument with the given index in the current function.
    Argument(usize),
    /// Local with the given index in the current function.
    LocalRegister(usize),
    /// Captured binding with the given index in a VM scope.
    Scope { scope_id: usize, index: usize },
    /// Binding is stored in the module's scope as an imported or exported binding.
    ModuleScope { scope_id: usize, index: usize },
    /// In sloppy evals vars are added to the parent var scope at runtime. This means they must be
    /// dynamically looked up by name at runtime, and are defined via special instructions.
    EvalVar,
    /// Vars defined in a with statement must be dynamically loaded and stored since they may be a
    /// normal var or they may be a property of the with object.
    WithVar,
}

pub struct VMScopeNode<'a> {
    bindings: AstSlice<'a, AstStr<'a>>,
}

impl<'a> VMScopeNode<'a> {
    #[inline]
    pub fn bindings(&self) -> &[AstStr<'a>] {
        &self.bindings
    }
}

pub static THIS_NAME: LazyLock<Wtf8String> = LazyLock::new(|| Wtf8String::from_str("this"));
pub static ARGUMENTS_NAME: LazyLock<Wtf8String> =
    LazyLock::new(|| Wtf8String::from_str("arguments"));
pub static DEFAULT_EXPORT_NAME: LazyLock<Wtf8String> =
    LazyLock::new(|| Wtf8String::from_str("default"));

pub static SHADOWED_SCOPE_SLOT_NAME: LazyLock<Wtf8String> =
    LazyLock::new(|| Wtf8String::from_str("%shadowed"));
pub static REALM_SCOPE_SLOT_NAME: LazyLock<Wtf8String> =
    LazyLock::new(|| Wtf8String::from_str("%realm"));
pub static MODULE_SCOPE_SLOT_NAME: LazyLock<Wtf8String> =
    LazyLock::new(|| Wtf8String::from_str("%module"));
pub static NEW_TARGET_BINDING_NAME: LazyLock<Wtf8String> =
    LazyLock::new(|| Wtf8String::from_str("%new.target"));
pub static DERIVED_CONSTRUCTOR_BINDING_NAME: LazyLock<Wtf8String> =
    LazyLock::new(|| Wtf8String::from_str("%constructor"));
pub static HOME_OBJECT_BINDING_NAME: LazyLock<Wtf8String> =
    LazyLock::new(|| Wtf8String::from_str("%homeObject"));
pub static STATIC_HOME_OBJECT_BINDING_NAME: LazyLock<Wtf8String> =
    LazyLock::new(|| Wtf8String::from_str("%staticHomeObject"));
pub static ANONYMOUS_DEFAULT_EXPORT_NAME: LazyLock<Wtf8String> =
    LazyLock::new(|| Wtf8String::from_str("%defaultExport"));
static CLASS_FIELD_SLOT_NAME: LazyLock<Wtf8String> =
    LazyLock::new(|| Wtf8String::from_str("%classField"));
