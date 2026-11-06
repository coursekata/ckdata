use arrow_array::RecordBatch;
use arrow_array::cast::AsArray;
use arrow_array::types::{Int64Type, TimestampMillisecondType};
use arrow_schema::{DataType, TimeUnit};

#[test]
fn arrow_probe_keeps_nulls_empties_and_int64() {
    let reader = ckdata::arrow_probe().expect("the probe round-trips through Parquet");
    let schema = reader.schema();
    let batches: Vec<RecordBatch> = reader.collect::<Result<_, _>>().unwrap();
    assert_eq!(batches.iter().map(RecordBatch::num_rows).sum::<usize>(), 2);
    let batch = arrow_select::concat::concat_batches(&schema, &batches).unwrap();
    let column = |name: &str| batch.column(schema.index_of(name).unwrap()).clone();
    assert_eq!(
        column("s_null")
            .as_string::<i32>()
            .iter()
            .collect::<Vec<_>>(),
        vec![None, Some("a")]
    );
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
    assert_eq!(
        column("ts")
            .as_primitive::<TimestampMillisecondType>()
            .iter()
            .collect::<Vec<_>>(),
        vec![Some(1_792_154_096_789), None]
    );
}
