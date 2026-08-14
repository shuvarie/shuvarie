pub trait ToDecSepNum {
    fn to_dec_sep_num(self, sep: char) -> String;
}

macro_rules! dec_sep_num_impl {
    ($typ: ty) => {
        impl ToDecSepNum for $typ {
            fn to_dec_sep_num(self, sep: char) -> String {
                let s = self.to_string();
                let mut result = String::new();
                for (i, c) in s.chars().rev().enumerate() {
                    if i > 0 && i % 3 == 0 {
                        result.push(sep);
                    }
                    result.push(c);
                }

                result.chars().rev().collect()
            }
        }
    };
    (#check_abs $typ: ty) => {
        impl ToDecSepNum for $typ {
            fn to_dec_sep_num(self, sep: char) -> String {
                let s = self.abs().to_string();
                let mut result = String::new();
                for (i, c) in s.chars().rev().enumerate() {
                    if i > 0 && i % 3 == 0 {
                        result.push(sep);
                    }
                    result.push(c);
                }
                let mut result: String = result.chars().rev().collect();
                if self < 0 {
                    result.insert(0, '-');
                }
                result
            }
        }
    };
}

dec_sep_num_impl!(#check_abs i8);
dec_sep_num_impl!(#check_abs i16);
dec_sep_num_impl!(#check_abs i32);
dec_sep_num_impl!(#check_abs i64);

dec_sep_num_impl!(u8);
dec_sep_num_impl!(u16);
dec_sep_num_impl!(u32);
dec_sep_num_impl!(u64);
