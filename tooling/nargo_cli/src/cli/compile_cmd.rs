use std::path::Path;

use acvm::acir::circuit::opcodes::BlackBoxFuncCall;
use acvm::acir::circuit::Opcode;
use acvm::Language;
use backend_interface::BackendOpcodeSupport;
use fm::FileManager;
use iter_extended::vecmap;
use nargo::artifacts::contract::PreprocessedContract;
use nargo::artifacts::contract::PreprocessedContractFunction;
use nargo::artifacts::debug::DebugArtifact;
use nargo::artifacts::program::PreprocessedProgram;
use nargo::package::Package;
use nargo::prepare_package;
use nargo::workspace::Workspace;
use nargo_toml::{get_package_manifest, resolve_workspace_from_toml, PackageSelection};
use noirc_driver::NOIR_ARTIFACT_VERSION_STRING;
use noirc_driver::{CompilationResult, CompileOptions, CompiledContract, CompiledProgram};
use noirc_frontend::graph::CrateName;

use clap::Args;

use crate::backends::Backend;
use crate::errors::{CliError, CompileError};

use super::fs::program::{
    read_debug_artifact_from_file, read_program_from_file, save_contract_to_file,
    save_debug_artifact_to_file, save_program_to_file,
};
use super::NargoConfig;
use rayon::prelude::*;

// TODO(#1388): pull this from backend.
const BACKEND_IDENTIFIER: &str = "acvm-backend-barretenberg";

/// Compile the program and its secret execution trace into ACIR format
#[derive(Debug, Clone, Args)]
pub(crate) struct CompileCommand {
    /// Include Proving and Verification keys in the build artifacts.
    #[arg(long)]
    include_keys: bool,

    /// The name of the package to compile
    #[clap(long, conflicts_with = "workspace")]
    package: Option<CrateName>,

    /// Compile all packages in the workspace
    #[clap(long, conflicts_with = "package")]
    workspace: bool,

    #[clap(flatten)]
    compile_options: CompileOptions,
}

pub(crate) fn run(
    backend: &Backend,
    args: CompileCommand,
    config: NargoConfig,
) -> Result<(), CliError> {
    let toml_path = get_package_manifest(&config.program_dir)?;
    let default_selection =
        if args.workspace { PackageSelection::All } else { PackageSelection::DefaultOrAll };
    let selection = args.package.map_or(default_selection, PackageSelection::Selected);
    let workspace = resolve_workspace_from_toml(&toml_path, selection)?;
    let circuit_dir = workspace.target_directory_path();

    let (binary_packages, contract_packages): (Vec<_>, Vec<_>) = workspace
        .into_iter()
        .filter(|package| !package.is_library())
        .cloned()
        .partition(|package| package.is_binary());

    let (np_language, opcode_support) = backend.get_backend_info()?;
    let (_, compiled_contracts) = compile_workspace(
        &workspace,
        &binary_packages,
        &contract_packages,
        np_language,
        &opcode_support,
        &args.compile_options,
    )?;

    // Save build artifacts to disk.
    for (package, contract) in contract_packages.into_iter().zip(compiled_contracts) {
        save_contract(contract, &package, &circuit_dir);
    }

    Ok(())
}

pub(super) fn compile_workspace(
    workspace: &Workspace,
    binary_packages: &[Package],
    contract_packages: &[Package],
    np_language: Language,
    opcode_support: &BackendOpcodeSupport,
    compile_options: &CompileOptions,
) -> Result<(Vec<CompiledProgram>, Vec<CompiledContract>), CliError> {
    let is_opcode_supported = |opcode: &_| opcode_support.is_opcode_supported(opcode);

    // Compile all of the packages in parallel.
    let program_results: Vec<(FileManager, CompilationResult<CompiledProgram>)> = binary_packages
        .par_iter()
        .map(|package| {
            compile_program(workspace, package, compile_options, np_language, &is_opcode_supported)
        })
        .collect();
    let contract_results: Vec<(FileManager, CompilationResult<CompiledContract>)> =
        contract_packages
            .par_iter()
            .map(|package| {
                compile_contract(package, compile_options, np_language, &is_opcode_supported)
            })
            .collect();

    // Report any warnings/errors which were encountered during compilation.
    let compiled_programs: Vec<CompiledProgram> = program_results
        .into_iter()
        .map(|(file_manager, compilation_result)| {
            report_errors(
                compilation_result,
                &file_manager,
                compile_options.deny_warnings,
                compile_options.silence_warnings,
            )
        })
        .collect::<Result<_, _>>()?;
    let compiled_contracts: Vec<CompiledContract> = contract_results
        .into_iter()
        .map(|(file_manager, compilation_result)| {
            report_errors(
                compilation_result,
                &file_manager,
                compile_options.deny_warnings,
                compile_options.silence_warnings,
            )
        })
        .collect::<Result<_, _>>()?;

    Ok((compiled_programs, compiled_contracts))
}

pub(crate) fn compile_bin_package(
    workspace: &Workspace,
    package: &Package,
    compile_options: &CompileOptions,
    np_language: Language,
    is_opcode_supported: &impl Fn(&Opcode) -> bool,
) -> Result<CompiledProgram, CliError> {
    if package.is_library() {
        return Err(CompileError::LibraryCrate(package.name.clone()).into());
    }

    let (file_manager, compilation_result) =
        compile_program(workspace, package, compile_options, np_language, &is_opcode_supported);

    let program = report_errors(
        compilation_result,
        &file_manager,
        compile_options.deny_warnings,
        compile_options.silence_warnings,
    )?;

    Ok(program)
}

fn compile_program(
    workspace: &Workspace,
    package: &Package,
    compile_options: &CompileOptions,
    np_language: Language,
    is_opcode_supported: &impl Fn(&Opcode) -> bool,
) -> (FileManager, CompilationResult<CompiledProgram>) {
    let (mut context, crate_id) =
        prepare_package(package, Box::new(|path| std::fs::read_to_string(path)));

    let program_artifact_path = workspace.package_build_path(package);
    let mut debug_artifact_path = program_artifact_path.clone();
    debug_artifact_path.set_file_name(format!("debug_{}.json", package.name));
    let cached_program = if let (Ok(preprocessed_program), Ok(mut debug_artifact)) = (
        read_program_from_file(program_artifact_path),
        read_debug_artifact_from_file(debug_artifact_path),
    ) {
        Some(CompiledProgram {
            hash: preprocessed_program.hash,
            circuit: preprocessed_program.bytecode,
            abi: preprocessed_program.abi,
            noir_version: preprocessed_program.noir_version,
            debug: debug_artifact.debug_symbols.remove(0),
            file_map: debug_artifact.file_map,
            warnings: debug_artifact.warnings,
        })
    } else {
        None
    };

    let force_recompile =
        cached_program.as_ref().map_or(false, |p| p.noir_version != NOIR_ARTIFACT_VERSION_STRING);
    let (program, warnings) = match noirc_driver::compile_main(
        &mut context,
        crate_id,
        compile_options,
        cached_program,
        force_recompile,
    ) {
        Ok(program_and_warnings) => program_and_warnings,
        Err(errors) => {
            return (context.file_manager, Err(errors));
        }
    };

    // TODO: we say that pedersen hashing is supported by all backends for now
    let is_opcode_supported_pedersen_hash = |opcode: &Opcode| -> bool {
        if let Opcode::BlackBoxFuncCall(BlackBoxFuncCall::PedersenHash { .. }) = opcode {
            true
        } else {
            is_opcode_supported(opcode)
        }
    };

    // Apply backend specific optimizations.
    let optimized_program =
        nargo::ops::optimize_program(program, np_language, &is_opcode_supported_pedersen_hash)
            .expect("Backend does not support an opcode that is in the IR");

    save_program(optimized_program.clone(), package, &workspace.target_directory_path());

    (context.file_manager, Ok((optimized_program, warnings)))
}

fn compile_contract(
    package: &Package,
    compile_options: &CompileOptions,
    np_language: Language,
    is_opcode_supported: &impl Fn(&Opcode) -> bool,
) -> (FileManager, CompilationResult<CompiledContract>) {
    let (mut context, crate_id) =
        prepare_package(package, Box::new(|path| std::fs::read_to_string(path)));
    let (contract, warnings) =
        match noirc_driver::compile_contract(&mut context, crate_id, compile_options) {
            Ok(contracts_and_warnings) => contracts_and_warnings,
            Err(errors) => {
                return (context.file_manager, Err(errors));
            }
        };

    let optimized_contract =
        nargo::ops::optimize_contract(contract, np_language, &is_opcode_supported)
            .expect("Backend does not support an opcode that is in the IR");

    (context.file_manager, Ok((optimized_contract, warnings)))
}

fn save_program(program: CompiledProgram, package: &Package, circuit_dir: &Path) {
    let preprocessed_program = PreprocessedProgram {
        hash: program.hash,
        backend: String::from(BACKEND_IDENTIFIER),
        abi: program.abi,
        noir_version: program.noir_version,
        bytecode: program.circuit,
    };

    save_program_to_file(&preprocessed_program, &package.name, circuit_dir);

    let debug_artifact = DebugArtifact {
        debug_symbols: vec![program.debug],
        file_map: program.file_map,
        warnings: program.warnings,
    };
    let circuit_name: String = (&package.name).into();
    save_debug_artifact_to_file(&debug_artifact, &circuit_name, circuit_dir);
}

fn save_contract(contract: CompiledContract, package: &Package, circuit_dir: &Path) {
    // TODO(#1389): I wonder if it is incorrect for nargo-core to know anything about contracts.
    // As can be seen here, It seems like a leaky abstraction where ContractFunctions (essentially CompiledPrograms)
    // are compiled via nargo-core and then the PreprocessedContract is constructed here.
    // This is due to EACH function needing it's own CRS, PKey, and VKey from the backend.
    let debug_artifact = DebugArtifact {
        debug_symbols: contract.functions.iter().map(|function| function.debug.clone()).collect(),
        file_map: contract.file_map,
        warnings: contract.warnings,
    };

    let preprocessed_functions = vecmap(contract.functions, |func| PreprocessedContractFunction {
        name: func.name,
        function_type: func.function_type,
        is_internal: func.is_internal,
        abi: func.abi,
        bytecode: func.bytecode,
    });

    let preprocessed_contract = PreprocessedContract {
        noir_version: contract.noir_version,
        name: contract.name,
        backend: String::from(BACKEND_IDENTIFIER),
        functions: preprocessed_functions,
        events: contract.events,
    };

    save_contract_to_file(
        &preprocessed_contract,
        &format!("{}-{}", package.name, preprocessed_contract.name),
        circuit_dir,
    );

    save_debug_artifact_to_file(
        &debug_artifact,
        &format!("{}-{}", package.name, preprocessed_contract.name),
        circuit_dir,
    );
}

/// Helper function for reporting any errors in a `CompilationResult<T>`
/// structure that is commonly used as a return result in this file.
pub(crate) fn report_errors<T>(
    result: CompilationResult<T>,
    file_manager: &FileManager,
    deny_warnings: bool,
    silence_warnings: bool,
) -> Result<T, CompileError> {
    let (t, warnings) = result.map_err(|errors| {
        noirc_errors::reporter::report_all(
            file_manager.as_file_map(),
            &errors,
            deny_warnings,
            silence_warnings,
        )
    })?;

    noirc_errors::reporter::report_all(
        file_manager.as_file_map(),
        &warnings,
        deny_warnings,
        silence_warnings,
    );

    Ok(t)
}
