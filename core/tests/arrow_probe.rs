use arrow_array::RecordBatch;
use arrow_array::cast::AsArray;
use arrow_array::types::Int64Type;
use arrow_schema::{DataType, TimeUnit};

#[test]
fn arrow_probe_keeps_nulls_empties_and_int64() {
    let reader = ckdata::arrow_probe().expect("the probe round-trips through Parquet");
    let schema = reader.schema();
    let batches: Vec<RecordBatch> = reader.collect::<Result<_, _>>().unwrap();
    assert_eq!(batches.iter().map(RecordBatch::num_rows).sum::<usize>(), 2);
    let batch = &batches[0];
    let column = |name: &str| batch.column(schema.index_of(name).unwrap()).clone();
    assert!(column("s_null").is_null(0), "a NULL string must stay NULL");
    assert_eq!(
        column("s_empty")
            .as_string::<i32>()
            .iter()
            .collect::<Vec<_>>(),
        vec![Some(""), Some("b")]
    );
    assert_eq!(
        column("big")
            .as_primitive::<Int64Type>()
            .iter()
            .collect::<Vec<_>>(),
        vec![Some(9_007_199_254_740_993), None]
    );
    assert_eq!(
        schema.field_with_name("ts").unwrap().data_type(),
        &DataType::Timestamp(TimeUnit::Millisecond, None)
    );
}
