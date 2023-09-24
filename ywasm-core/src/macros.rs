#[macro_export]
macro_rules! branch_abi {
    ($t:ty, $tref:ty) => {
        impl wasm_bindgen::describe::WasmDescribe for $t {
            fn describe() {
                wasm_bindgen::describe::inform(wasm_bindgen::describe::RUST_STRUCT);
                for c in "stringify($t)".chars() {
                    wasm_bindgen::describe::inform(c as u32);
                }
            }
        }

        impl wasm_bindgen::convert::FromWasmAbi for $t {
            type Abi = u32;

            unsafe fn from_abi(js: Self::Abi) -> Self {
                let branch = crate::branch_ref::BranchRef::from_abi(js);
                <$t>::new(<$tref>::from(branch.into_ptr()))
            }
        }

        impl wasm_bindgen::convert::IntoWasmAbi for $t {
            type Abi = u32;

            fn into_abi(self) -> Self::Abi {
                let branch = crate::branch_ref::BranchRef::from(self.0);
                branch.into_abi()
            }
        }

        impl std::ops::Deref for $t {
            type Target = $tref;

            #[inline]
            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl From<$tref> for $t {
            #[inline]
            fn from(value: $tref) -> Self {
                <$t>::new(value)
            }
        }
    };
}
