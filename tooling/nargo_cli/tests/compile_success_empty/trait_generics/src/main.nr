
struct Empty<T> {}

trait Foo {
    fn foo(self) -> u32;
}

impl Foo for Empty<u32> {
    fn foo(_self: Self) -> u32 { 32 }
}

impl Foo for Empty<u64> {
    fn foo(_self: Self) -> u32 { 64 }
}

fn main() {
    let x: Empty<u32> = Empty {};
    let y: Empty<u64> = Empty {};
    let z = Empty {};

    assert(x.foo() == 32);
    assert(y.foo() == 64);

    // Types matching multiple impls will currently choose 
    // the first matching one instead of erroring
    assert(z.foo() == 32);

    // Ensure we can call a generic impl
    let x: u8 = 7;
    let y: i8 = 8;
    let s2_u8 = S2 { x };
    let s2_i8 = S2 { x: y };
    assert(s2_u8.t2().x == 7);
    assert(s2_i8.t2().x == 8);
}

trait T2 {
    fn t2(self) -> Self;
}

struct S2<T> { x: T }

impl<T> T2 for S2<T> {
    fn t2(self) -> Self { self }
}
