pub fn from() { let _ = <rmux_proto::RmuxError as From<rmux_sdk::SessionNameError>>::from(rmux_sdk::SessionNameError); }
pub fn into() { let _: rmux_proto::RmuxError = rmux_sdk::SessionName::new("").unwrap_err().into(); }
pub fn try_into() { let _: Result<rmux_proto::RmuxError, std::convert::Infallible> = rmux_sdk::SessionNameError.try_into(); }
pub fn question_mark() -> Result<rmux_sdk::SessionName, rmux_proto::RmuxError> { Ok(rmux_sdk::SessionName::new("")?) }
