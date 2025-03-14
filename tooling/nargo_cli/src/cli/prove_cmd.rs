use clap::Args;
use nargo::constants::{PROVER_INPUT_FILE, VERIFIER_INPUT_FILE};
use nargo::package::Package;
use nargo::workspace::Workspace;
use nargo_toml::{get_package_manifest, resolve_workspace_from_toml, PackageSelection};
use noirc_abi::input_parser::Format;
use noirc_driver::{CompileOptions, CompiledProgram};
use noirc_frontend::graph::CrateName;

use super::compile_cmd::compile_bin_package;
use super::fs::{
    inputs::{read_inputs_from_file, write_inputs_to_file},
    proof::save_proof_to_dir,
};
use super::NargoConfig;
use crate::{backends::Backend, cli::execute_cmd::execute_program, errors::CliError};

/// Create proof for this program. The proof is returned as a hex encoded string.
#[derive(Debug, Clone, Args)]
pub(crate) struct ProveCommand {
    /// The name of the toml file which contains the inputs for the prover
    #[clap(long, short, default_value = PROVER_INPUT_FILE)]
    prover_name: String,

    /// The name of the toml file which contains the inputs for the verifier
    #[clap(long, short, default_value = VERIFIER_INPUT_FILE)]
    verifier_name: String,

    /// Verify proof after proving
    #[arg(long)]
    verify: bool,

    /// The name of the package to prove
    #[clap(long, conflicts_with = "workspace")]
    package: Option<CrateName>,

    /// Prove all packages in the workspace
    #[clap(long, conflicts_with = "package")]
    workspace: bool,

    #[clap(flatten)]
    compile_options: CompileOptions,
}

pub(crate) fn run(
    backend: &Backend,
    args: ProveCommand,
    config: NargoConfig,
) -> Result<(), CliError> {
    let toml_path = get_package_manifest(&config.program_dir)?;
    let default_selection =
        if args.workspace { PackageSelection::All } else { PackageSelection::DefaultOrAll };
    let selection = args.package.map_or(default_selection, PackageSelection::Selected);
    let workspace = resolve_workspace_from_toml(&toml_path, selection)?;

    let (np_language, opcode_support) = backend.get_backend_info()?;
    for package in &workspace {
        let program = compile_bin_package(
            &workspace,
            package,
            &args.compile_options,
            np_language,
            &|opcode| opcode_support.is_opcode_supported(opcode),
        )?;

        prove_package(
            backend,
            &workspace,
            package,
            program,
            &args.prover_name,
            &args.verifier_name,
            args.verify,
        )?;
    }

    Ok(())
}

pub(crate) fn prove_package(
    backend: &Backend,
    workspace: &Workspace,
    package: &Package,
    compiled_program: CompiledProgram,
    prover_name: &str,
    verifier_name: &str,
    check_proof: bool,
) -> Result<(), CliError> {
    // Parse the initial witness values from Prover.toml
    let (inputs_map, _) =
        read_inputs_from_file(&package.root_dir, prover_name, Format::Toml, &compiled_program.abi)?;

    let solved_witness = execute_program(&compiled_program, &inputs_map)?;

    // Write public inputs into Verifier.toml
    let public_abi = compiled_program.abi.public_abi();
    let (public_inputs, return_value) = public_abi.decode(&solved_witness)?;

    write_inputs_to_file(
        &public_inputs,
        &return_value,
        &public_abi,
        &package.root_dir,
        verifier_name,
        Format::Toml,
    )?;

    let proof = backend.prove(&compiled_program.circuit, solved_witness, false)?;

    if check_proof {
        let public_inputs = public_abi.encode(&public_inputs, return_value)?;
        let valid_proof =
            backend.verify(&proof, public_inputs, &compiled_program.circuit, false)?;

        if !valid_proof {
            return Err(CliError::InvalidProof("".into()));
        }
    }

    save_proof_to_dir(&proof, &String::from(&package.name), workspace.proofs_directory_path())?;

    Ok(())
}
