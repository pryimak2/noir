mod foo;
mod baz;

global M: Field = 32;
global L: Field = 10; // Unused globals currently allowed
global N: Field = 5;
global T_LEN = 2; // Type inference is allowed on globals
//global N: Field = 5; // Uncomment to see duplicate globals error 

struct Dummy {
     x: [Field; N],
     y: [Field; foo::MAGIC_NUMBER]
}

struct Test {
    v: Field,
}
global VALS: [Test; 1] = [Test { v: 100 }];
global NESTED = [VALS, VALS];

fn main(a: [Field; M + N - N], b: [Field; 30 + N / 2], c : pub [Field; foo::MAGIC_NUMBER], d: [Field; foo::bar::N]) {
    let test_struct = Dummy { x: d, y: c };

     for i in 0..foo::MAGIC_NUMBER {
          assert(c[i] == foo::MAGIC_NUMBER);
          assert(test_struct.y[i] == foo::MAGIC_NUMBER);
          assert(test_struct.y[i] != NESTED[1][0].v);
     }

    assert(N != M);

    let expected: u32 = 42;
    assert(foo::TYPE_INFERRED == expected);

    let mut y = 5;
    let mut x = M;
     for i in 0..N*N {
          let M: Field = 10;
          x = M;
          
          y = i;
     }
    assert(y == 24);
    assert(x == 10);

    let q = multiplyByM(3);
    assert(q == 96);

    arrays_neq(a, b);

    let t: [Field; T_LEN] = [N, M];
    assert(t[1] == 32);

    assert(15 == mysubmodule::my_helper());

    let add_submodules_N = mysubmodule::N + foo::bar::N;
    assert(15 == add_submodules_N);
    let add_from_bar_N = mysubmodule::N + foo::bar::from_bar(1);
    assert(15 == add_from_bar_N);

     // Example showing an array filled with (mysubmodule::N + 2) 0's
    let sugared = [0; mysubmodule::N + 2];
    assert(sugared[mysubmodule::N + 1] == 0);

    let arr: [Field; mysubmodule::N] = [N; 10];
    assert((arr[0] == 5) & (arr[9] == 5));

    foo::from_foo(d);
    baz::from_baz(c);
}

fn multiplyByM(x: Field) -> Field {
    x * M
}

fn arrays_neq(a: [Field; M], b: [Field; M]) {
    assert(a != b);
}

mod mysubmodule {
     global N: Field = 10;
     global L: Field = 50;

     fn my_bool_or(x: u1, y: u1) {
          assert(x | y == 1);
     }

     pub fn my_helper() -> Field {
          let N: Field = 15; // Like in Rust, local variables override globals
          let x = N;
          x
     }
}
