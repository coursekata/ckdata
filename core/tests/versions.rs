#[test]
fn versions_name_the_core_first() {
    assert_eq!(
        ckdata::versions()[0],
        format!("ckdata {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn versions_name_the_sdk_crates_without_configuring_a_client() {
    assert_eq!(
        ckdata::versions()[1..],
        [
            format!("aws-sdk-athena {}", aws_sdk_athena::meta::PKG_VERSION),
            format!("aws-sdk-s3 {}", aws_sdk_s3::meta::PKG_VERSION),
        ]
    );
}
