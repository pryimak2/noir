#![forbid(unsafe_code)]
#![warn(unused_crate_dependencies, unused_extern_crates)]
#![warn(unreachable_pub)]
#![warn(clippy::semicolon_if_nothing_returned)]

use clap::Args;
use debug::filter_relevant_files;
use fm::FileId;
use iter_extended::vecmap;
use noirc_abi::{AbiParameter, AbiType, ContractEvent};
use noirc_errors::{CustomDiagnostic, FileDiagnostic};
use noirc_evaluator::errors::RuntimeError;
use noirc_evaluator::{create_circuit, into_abi_params};
use noirc_frontend::graph::{CrateId, CrateName};
use noirc_frontend::hir::def_map::{Contract, CrateDefMap};
use noirc_frontend::hir::Context;
use noirc_frontend::monomorphization::monomorphize;
use noirc_frontend::node_interner::FuncId;
use serde::{Deserialize, Serialize};
use std::path::Path;

mod contract;
mod debug;
mod program;

pub use contract::{CompiledContract, ContractFunction, ContractFunctionType};
pub use debug::DebugFile;
pub use program::CompiledProgram;

const STD_CRATE_NAME: &str = "std";

pub const GIT_COMMIT: &str = env!("GIT_COMMIT");
pub const GIT_DIRTY: &str = env!("GIT_DIRTY");
pub const NOIRC_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Version string that gets placed in artifacts that Noir builds. This is semver compatible.
/// Note: You can't directly use the value of a constant produced with env! inside a concat! macro.
pub const NOIR_ARTIFACT_VERSION_STRING: &str =
    concat!(env!("CARGO_PKG_VERSION"), "+", env!("GIT_COMMIT"));

#[derive(Args, Clone, Debug, Default, Serialize, Deserialize)]
pub struct CompileOptions {
    /// Emit debug information for the intermediate SSA IR
    #[arg(long, hide = true)]
    pub show_ssa: bool,

    #[arg(long, hide = true)]
    pub show_brillig: bool,

    /// Display the ACIR for compiled circuit
    #[arg(long)]
    pub print_acir: bool,

    /// Treat all warnings as errors
    #[arg(long, conflicts_with = "silence_warnings")]
    pub deny_warnings: bool,

    /// Suppress warnings
    #[arg(long, conflicts_with = "deny_warnings")]
    pub silence_warnings: bool,
}

/// Helper type used to signify where only warnings are expected in file diagnostics
pub type Warnings = Vec<FileDiagnostic>;

/// Helper type used to signify where errors or warnings are expected in file diagnostics
pub type ErrorsAndWarnings = Vec<FileDiagnostic>;

/// Helper type for connecting a compilation artifact to the errors or warnings which were produced during compilation.
pub type CompilationResult<T> = Result<(T, Warnings), ErrorsAndWarnings>;

// This is here for backwards compatibility
// with the restricted version which only uses one file
pub fn compile_file(context: &mut Context, root_file: &Path) -> CompilationResult<CompiledProgram> {
    let crate_id = prepare_crate(context, root_file);
    compile_main(context, crate_id, &CompileOptions::default(), None, true)
}

/// Adds the file from the file system at `Path` to the crate graph as a root file
pub fn prepare_crate(context: &mut Context, file_name: &Path) -> CrateId {
    let path_to_std_lib_file = Path::new(STD_CRATE_NAME).join("lib.nr");
    let std_file_id = context.file_manager.add_file(&path_to_std_lib_file).unwrap();
    let std_crate_id = context.crate_graph.add_stdlib(std_file_id);

    let root_file_id = context.file_manager.add_file(file_name).unwrap();

    let root_crate_id = context.crate_graph.add_crate_root(root_file_id);

    add_dep(context, root_crate_id, std_crate_id, STD_CRATE_NAME.parse().unwrap());

    root_crate_id
}

// Adds the file from the file system at `Path` to the crate graph
pub fn prepare_dependency(context: &mut Context, file_name: &Path) -> CrateId {
    let root_file_id = context.file_manager.add_file(file_name).unwrap();

    let crate_id = context.crate_graph.add_crate(root_file_id);

    // Every dependency has access to stdlib
    let std_crate_id = context.stdlib_crate_id();
    add_dep(context, crate_id, *std_crate_id, STD_CRATE_NAME.parse().unwrap());

    crate_id
}

/// Adds a edge in the crate graph for two crates
pub fn add_dep(
    context: &mut Context,
    this_crate: CrateId,
    depends_on: CrateId,
    crate_name: CrateName,
) {
    context
        .crate_graph
        .add_dep(this_crate, crate_name, depends_on)
        .expect("cyclic dependency triggered");
}

/// Run the lexing, parsing, name resolution, and type checking passes.
///
/// This returns a (possibly empty) vector of any warnings found on success.
/// On error, this returns a non-empty vector of warnings and error messages, with at least one error.
pub fn check_crate(
    context: &mut Context,
    crate_id: CrateId,
    deny_warnings: bool,
) -> CompilationResult<()> {
    let mut errors = vec![];
    let diagnostics = CrateDefMap::collect_defs(crate_id, context);
    errors.extend(diagnostics.into_iter().map(|(error, file_id)| {
        let diagnostic: CustomDiagnostic = error.into();
        diagnostic.in_file(file_id)
    }));

    if has_errors(&errors, deny_warnings) {
        Err(errors)
    } else {
        Ok(((), errors))
    }
}

pub fn compute_function_abi(
    context: &Context,
    crate_id: &CrateId,
) -> Option<(Vec<AbiParameter>, Option<AbiType>)> {
    let main_function = context.get_main_function(crate_id)?;

    let func_meta = context.def_interner.function_meta(&main_function);

    let (parameters, return_type) = func_meta.into_function_signature();
    let parameters = into_abi_params(context, parameters);
    let return_type = return_type.map(|typ| AbiType::from_type(context, &typ));
    Some((parameters, return_type))
}

/// Run the frontend to check the crate for errors then compile the main function if there were none
///
/// On success this returns the compiled program alongside any warnings that were found.
/// On error this returns the non-empty list of warnings and errors.
pub fn compile_main(
    context: &mut Context,
    crate_id: CrateId,
    options: &CompileOptions,
    cached_program: Option<CompiledProgram>,
    force_compile: bool,
) -> CompilationResult<CompiledProgram> {
    let (_, mut warnings) = check_crate(context, crate_id, options.deny_warnings)?;

    let main = match context.get_main_function(&crate_id) {
        Some(m) => m,
        None => {
            // TODO(#2155): This error might be a better to exist in Nargo
            let err = CustomDiagnostic::from_message(
                "cannot compile crate into a program as it does not contain a `main` function",
            )
            .in_file(FileId::default());
            return Err(vec![err]);
        }
    };

    let compiled_program = compile_no_check(context, options, main, cached_program, force_compile)
        .map_err(FileDiagnostic::from)?;
    let compilation_warnings = vecmap(compiled_program.warnings.clone(), FileDiagnostic::from);
    if options.deny_warnings && !compilation_warnings.is_empty() {
        return Err(compilation_warnings);
    }
    warnings.extend(compilation_warnings);

    if options.print_acir {
        println!("Compiled ACIR for main (unoptimized):");
        println!("{}", compiled_program.circuit);
    }

    Ok((compiled_program, warnings))
}

/// Run the frontend to check the crate for errors then compile all contracts if there were none
pub fn compile_contract(
    context: &mut Context,
    crate_id: CrateId,
    options: &CompileOptions,
) -> CompilationResult<CompiledContract> {
    let (_, warnings) = check_crate(context, crate_id, options.deny_warnings)?;

    // TODO: We probably want to error if contracts is empty
    let contracts = context.get_all_contracts(&crate_id);

    let mut compiled_contracts = vec![];
    let mut errors = warnings;

    if contracts.len() > 1 {
        let err = CustomDiagnostic::from_message("Packages are limited to a single contract")
            .in_file(FileId::default());
        return Err(vec![err]);
    } else if contracts.is_empty() {
        let err = CustomDiagnostic::from_message(
            "cannot compile crate into a contract as it does not contain any contracts",
        )
        .in_file(FileId::default());
        return Err(vec![err]);
    };

    for contract in contracts {
        match compile_contract_inner(context, contract, options) {
            Ok(contract) => compiled_contracts.push(contract),
            Err(mut more_errors) => errors.append(&mut more_errors),
        }
    }

    if has_errors(&errors, options.deny_warnings) {
        Err(errors)
    } else {
        assert_eq!(compiled_contracts.len(), 1);
        let compiled_contract = compiled_contracts.remove(0);

        if options.print_acir {
            for contract_function in &compiled_contract.functions {
                println!(
                    "Compiled ACIR for {}::{} (unoptimized):",
                    compiled_contract.name, contract_function.name
                );
                println!("{}", contract_function.bytecode);
            }
        }
        // errors here is either empty or contains only warnings
        Ok((compiled_contract, errors))
    }
}

/// True if there are (non-warning) errors present and we should halt compilation
fn has_errors(errors: &[FileDiagnostic], deny_warnings: bool) -> bool {
    if deny_warnings {
        !errors.is_empty()
    } else {
        errors.iter().any(|error| error.diagnostic.is_error())
    }
}

/// Compile all of the functions associated with a Noir contract.
fn compile_contract_inner(
    context: &Context,
    contract: Contract,
    options: &CompileOptions,
) -> Result<CompiledContract, ErrorsAndWarnings> {
    let mut functions = Vec::new();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    for contract_function in &contract.functions {
        let function_id = contract_function.function_id;
        let is_entry_point = contract_function.is_entry_point;

        let name = context.function_name(&function_id).to_owned();

        // We assume that functions have already been type checked.
        // This is the exact same assumption that compile_no_check makes.
        // If it is not an entry-point point, we can then just skip the
        // compilation step. It will also not be added to the ABI.
        if !is_entry_point {
            continue;
        }

        let function = match compile_no_check(context, options, function_id, None, true) {
            Ok(function) => function,
            Err(new_error) => {
                errors.push(FileDiagnostic::from(new_error));
                continue;
            }
        };
        warnings.extend(function.warnings);
        let modifiers = context.def_interner.function_modifiers(&function_id);
        let func_type = modifiers
            .contract_function_type
            .expect("Expected contract function to have a contract visibility");

        let function_type = ContractFunctionType::new(func_type, modifiers.is_unconstrained);

        functions.push(ContractFunction {
            name,
            function_type,
            is_internal: modifiers.is_internal.unwrap_or(false),
            abi: function.abi,
            bytecode: function.circuit,
            debug: function.debug,
        });
    }

    if errors.is_empty() {
        let debug_infos: Vec<_> = functions.iter().map(|function| function.debug.clone()).collect();
        let file_map = filter_relevant_files(&debug_infos, &context.file_manager);

        Ok(CompiledContract {
            name: contract.name,
            events: contract
                .events
                .iter()
                .map(|event_id| {
                    let typ = context.def_interner.get_struct(*event_id);
                    let typ = typ.borrow();
                    ContractEvent::from_struct_type(context, &typ)
                })
                .collect(),
            functions,
            file_map,
            noir_version: NOIR_ARTIFACT_VERSION_STRING.to_string(),
            warnings,
        })
    } else {
        Err(errors)
    }
}

/// Compile the current crate. Assumes self.check_crate is called beforehand!
///
/// This function also assumes all errors in experimental_create_circuit and create_circuit
/// are not warnings.
#[allow(deprecated)]
pub fn compile_no_check(
    context: &Context,
    options: &CompileOptions,
    main_function: FuncId,
    cached_program: Option<CompiledProgram>,
    force_compile: bool,
) -> Result<CompiledProgram, RuntimeError> {
    let program = monomorphize(main_function, &context.def_interner);

    let hash = fxhash::hash64(&program);

    // If user has specified that they want to see intermediate steps printed then we should
    // force compilation even if the program hasn't changed.
    if !(force_compile || options.print_acir || options.show_brillig || options.show_ssa) {
        if let Some(cached_program) = cached_program {
            if hash == cached_program.hash {
                return Ok(cached_program);
            }
        }
    }

    let (circuit, debug, abi, warnings) =
        create_circuit(context, program, options.show_ssa, options.show_brillig)?;

    let file_map = filter_relevant_files(&[debug.clone()], &context.file_manager);

    Ok(CompiledProgram {
        hash,
        circuit,
        debug,
        abi,
        file_map,
        noir_version: NOIR_ARTIFACT_VERSION_STRING.to_string(),
        warnings,
    })
}
