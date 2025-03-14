use dep::std;

fn main(
  // Public key of note
  // all notes have the same denomination
  priv_key: Field, 

  // Merkle membership proof
  note_root: pub Field, 
  index: Field,
  note_hash_path: [Field; 3],

  // Receiver public key
  to_pubkey_x: Field, 
  to_pubkey_y: Field, 
) -> pub [Field; 2] {
    // Compute public key from private key to show ownership
    let pubkey = std::scalar_mul::fixed_base_embedded_curve(priv_key, 0);
    let pubkey_x = pubkey[0];
    let pubkey_y = pubkey[1];

    // Compute input note commitment
    let note_commitment = std::hash::pedersen_commitment([pubkey_x, pubkey_y]);
    
    // Compute input note nullifier
    let nullifier = std::hash::pedersen_commitment([note_commitment[0], index, priv_key]);

    // Compute output note nullifier
    let receiver_note_commitment = std::hash::pedersen_commitment([to_pubkey_x, to_pubkey_y]);

    // Check that the input note nullifier is in the root
    assert(note_root == std::merkle::compute_merkle_root(note_commitment[0], index, note_hash_path));

    [nullifier[0], receiver_note_commitment[0]]
}
