use dep::std;

fn main(x: Field, y: Field, salt: Field, out_x: Field, out_y: Field, out_hash: Field) {
    let res = std::hash::pedersen_commitment([x, y]);
    assert(res[0] == out_x);
    assert(res[1] == out_y);
    let res_hash = std::hash::pedersen_hash_with_separator([x, y], 0);
    assert_eq(res_hash, out_hash);

    assert(res_hash != res[0]);

    let raw_data = [x, y];
    let mut state = 0;
    for i in 0..2 {
        state = state * 8 + raw_data[i];
    }
    state += salt;
    let hash = std::hash::pedersen_commitment([state]);
    assert(std::hash::pedersen_commitment([43])[0] == hash[0]);
}

