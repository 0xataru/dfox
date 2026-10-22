//! Writing a query's rows to a file as they stream in, and reading rows
//! from a file to import: CSV, TSV, JSON Lines or Parquet, picked by the
//! file's extension.

use std::{
    fs::File,
    io::{BufReader, BufWriter},
    path::Path,
    sync::Arc,
};

use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;

use crate::errors::DbError;

/// A file format rows can be written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Csv,
    Tsv,
    JsonLines,
    Parquet,
}

impl Format {
    /// The format a file named `path` is written in, by its extension.
    pub fn of(path: &Path) -> Result<Self, String> {
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .unwrap_or_default();
        match extension.as_str() {
            "csv" => Ok(Format::Csv),
            "tsv" | "tab" => Ok(Format::Tsv),
            "jsonl" | "ndjson" => Ok(Format::JsonLines),
            "parquet" => Ok(Format::Parquet),
            _ => Err(format!(
                "{}: name the file .csv, .tsv, .jsonl or .parquet",
                path.display()
            )),
        }
    }
}

fn io_error(path: &Path, err: impl std::fmt::Display) -> DbError {
    DbError::General(format!("{}: {err}", path.display()))
}

/// Writes batches of one schema to a file.
pub enum Writer {
    Csv(arrow_csv::Writer<BufWriter<File>>),
    Json(arrow_json::LineDelimitedWriter<BufWriter<File>>),
    Parquet(parquet::arrow::ArrowWriter<File>),
}

impl Writer {
    /// Creates `path`, replacing it, for rows of `schema`.
    pub fn create(path: &Path, schema: &SchemaRef) -> Result<Self, DbError> {
        let format = Format::of(path).map_err(DbError::Config)?;
        let file = File::create(path).map_err(|err| io_error(path, err))?;
        Ok(match format {
            Format::Csv | Format::Tsv => {
                let delimiter = if format == Format::Tsv { b'\t' } else { b',' };
                Writer::Csv(
                    arrow_csv::WriterBuilder::new()
                        .with_header(true)
                        .with_delimiter(delimiter)
                        .build(BufWriter::new(file)),
                )
            }
            Format::JsonLines => {
                Writer::Json(arrow_json::LineDelimitedWriter::new(BufWriter::new(file)))
            }
            Format::Parquet => Writer::Parquet(
                parquet::arrow::ArrowWriter::try_new(file, schema.clone(), None)
                    .map_err(|err| io_error(path, err))?,
            ),
        })
    }

    pub fn write(&mut self, batch: &RecordBatch) -> Result<(), DbError> {
        let result = match self {
            Writer::Csv(w) => w.write(batch).map_err(|e| e.to_string()),
            Writer::Json(w) => w.write(batch).map_err(|e| e.to_string()),
            Writer::Parquet(w) => w.write(batch).map_err(|e| e.to_string()),
        };
        result.map_err(DbError::General)
    }

    /// Writes what is buffered and the file's footer.
    pub fn finish(self) -> Result<(), DbError> {
        let result = match self {
            Writer::Csv(w) => w
                .into_inner()
                .into_inner()
                .map(drop)
                .map_err(|e| e.to_string()),
            Writer::Json(mut w) => w.finish().map_err(|e| e.to_string()),
            Writer::Parquet(w) => w.close().map(drop).map_err(|e| e.to_string()),
        };
        result.map_err(DbError::General)
    }
}

/// Rows of a file, a batch at a time.
pub type Batches = Box<dyn Iterator<Item = Result<RecordBatch, DbError>> + Send>;

/// Reads the rows of `path`, in the format its extension names. Columns of
/// CSV and JSON get their types from the first rows.
pub fn read(path: &Path) -> Result<Batches, DbError> {
    const INFER_FROM: Option<usize> = Some(1000);
    let format = Format::of(path).map_err(DbError::Config)?;
    let open = || File::open(path).map_err(|err| io_error(path, err));
    let owned = path.to_path_buf();
    let batches = move |err| io_error(&owned, err);
    Ok(match format {
        Format::Csv | Format::Tsv => {
            let delimiter = if format == Format::Tsv { b'\t' } else { b',' };
            let (schema, _) = arrow_csv::reader::Format::default()
                .with_header(true)
                .with_delimiter(delimiter)
                .infer_schema(open()?, INFER_FROM)
                .map_err(|err| io_error(path, err))?;
            let reader = arrow_csv::ReaderBuilder::new(Arc::new(schema))
                .with_header(true)
                .with_delimiter(delimiter)
                .build(open()?)
                .map_err(|err| io_error(path, err))?;
            Box::new(reader.map(move |b| b.map_err(&batches)))
        }
        Format::JsonLines => {
            let (schema, _) = arrow_json::reader::infer_json_schema_from_seekable(
                BufReader::new(open()?),
                INFER_FROM,
            )
            .map_err(|err| io_error(path, err))?;
            let reader = arrow_json::ReaderBuilder::new(Arc::new(schema))
                .build(BufReader::new(open()?))
                .map_err(|err| io_error(path, err))?;
            Box::new(reader.map(move |b| b.map_err(&batches)))
        }
        Format::Parquet => {
            let reader =
                parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(open()?)
                    .map_err(|err| io_error(path, err))?
                    .build()
                    .map_err(|err| io_error(path, err))?;
            Box::new(reader.map(move |b| b.map_err(&batches)))
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::{Int64Array, StringArray};
    use arrow_schema::{DataType, Field, Schema};

    use super::*;

    fn batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, true),
        ]));
        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int64Array::from(vec![1, 2])),
                Arc::new(StringArray::from(vec![Some("a, b"), None])),
            ],
        )
        .expect("batch")
    }

    fn written(name: &str) -> (std::path::PathBuf, Vec<u8>) {
        let path = std::env::temp_dir().join(format!("dfox-export-{}-{name}", std::process::id()));
        let batch = batch();
        let mut writer = Writer::create(&path, &batch.schema()).expect("create");
        writer.write(&batch).expect("write");
        writer.finish().expect("finish");
        let bytes = std::fs::read(&path).expect("read");
        (path, bytes)
    }

    #[test]
    fn formats_follow_the_extension() {
        assert_eq!(Format::of(Path::new("a.CSV")), Ok(Format::Csv));
        assert_eq!(Format::of(Path::new("a.ndjson")), Ok(Format::JsonLines));
        assert!(Format::of(Path::new("a.xlsx"))
            .unwrap_err()
            .contains(".parquet"));
    }

    #[test]
    fn text_formats_hold_the_rows() {
        let (path, csv) = written("rows.csv");
        assert_eq!(String::from_utf8_lossy(&csv), "id,name\n1,\"a, b\"\n2,\n");
        let _ = std::fs::remove_file(path);
        let (path, tsv) = written("rows.tsv");
        assert_eq!(String::from_utf8_lossy(&tsv), "id\tname\n1\ta, b\n2\t\n");
        let _ = std::fs::remove_file(path);
        let (path, json) = written("rows.jsonl");
        assert_eq!(
            String::from_utf8_lossy(&json),
            "{\"id\":1,\"name\":\"a, b\"}\n{\"id\":2}\n"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn every_format_reads_back_what_it_wrote() {
        for name in ["back.csv", "back.tsv", "back.jsonl", "back.parquet"] {
            let (path, _) = written(name);
            let batches: Vec<RecordBatch> = read(&path)
                .expect("read")
                .collect::<Result<_, _>>()
                .expect("batches");
            let rows: usize = batches.iter().map(RecordBatch::num_rows).sum();
            assert_eq!(rows, 2, "{name}");
            let names: Vec<String> = batches[0]
                .schema()
                .fields()
                .iter()
                .map(|f| f.name().clone())
                .collect();
            assert_eq!(names, ["id", "name"], "{name}");
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn parquet_reads_back() {
        let (path, bytes) = written("rows.parquet");
        assert_eq!(&bytes[..4], b"PAR1");
        let file = File::open(&path).expect("open");
        let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)
            .expect("reader")
            .build()
            .expect("build");
        let rows: usize = reader.map(|b| b.expect("batch").num_rows()).sum();
        assert_eq!(rows, 2);
        let _ = std::fs::remove_file(path);
    }
}
