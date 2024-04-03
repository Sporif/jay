macro_rules! tassert {
    ($cond:expr) => {
        if !$cond {
            bail!(
                "Assert `{}` failed ({}:{})",
                stringify!($cond),
                file!(),
                line!()
            );
        }
    };
}

macro_rules! tassert_eq {
    ($left:expr, $right:expr) => {{
        match ($left, $right) {
            (left, right) => {
                if left != right {
                    bail!(
                        "Assert `{} = {:?} = {:?} = {}` failed ({}:{})",
                        stringify!($left),
                        left,
                        right,
                        stringify!($right),
                        file!(),
                        line!()
                    );
                }
            }
        }
    }};
}
