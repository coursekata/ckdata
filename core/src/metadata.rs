use aws_sdk_athena::error::ProvideErrorMetadata;

use crate::{Connection, Error};

const CATALOG: &str = "AwsDataCatalog";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnMetadata {
    pub name: String,
    /// Athena's type description is retained without guessing an Arrow type.
    pub data_type: Option<String>,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableMetadata {
    pub name: String,
    pub table_type: Option<String>,
    pub columns: Vec<ColumnMetadata>,
    pub partition_keys: Vec<ColumnMetadata>,
}

/// At most fifty tables. The caller controls how many pages to retain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TablePage {
    pub tables: Vec<TableMetadata>,
    pub next_token: Option<String>,
}

impl Connection {
    /// Inspect this connection's workgroup configuration without submitting SQL.
    /// Harnesses can verify their expected output and scan limits before querying.
    pub async fn workgroup_configuration(
        &self,
    ) -> Result<aws_sdk_athena::types::WorkGroupConfiguration, Error> {
        let client = aws_sdk_athena::Client::from_conf(
            aws_sdk_athena::config::Builder::from(self.aws_config())
                .timeout_config(
                    aws_sdk_athena::config::timeout::TimeoutConfig::builder()
                        .operation_timeout(self.request_timeout(std::time::Duration::from_secs(10)))
                        .build(),
                )
                .build(),
        );
        let response = client
            .get_work_group()
            .work_group(&self.options().workgroup)
            .send()
            .await
            .map_err(|error| Error::Service {
                operation: "GetWorkGroup",
                code: error
                    .as_service_error()
                    .and_then(|service| service.code())
                    .unwrap_or("request failed")
                    .to_owned(),
                message: crate::error::service_message(error.as_service_error()),
            })?;
        let group = response
            .work_group()
            .filter(|group| group.state() == Some(&aws_sdk_athena::types::WorkGroupState::Enabled))
            .ok_or(Error::InvalidServiceResponse("GetWorkGroup"))?;
        group
            .configuration()
            .cloned()
            .ok_or(Error::InvalidServiceResponse("GetWorkGroup"))
    }

    /// Discover table metadata without submitting a query or fetching result rows.
    pub async fn table_page(&self, next_token: Option<&str>) -> Result<TablePage, Error> {
        let response = aws_sdk_athena::Client::new(self.aws_config())
            .list_table_metadata()
            .catalog_name(CATALOG)
            .database_name(&self.options().schema)
            .work_group(&self.options().workgroup)
            .max_results(50)
            .set_next_token(next_token.map(str::to_owned))
            .send()
            .await
            .map_err(|error| Error::Service {
                operation: "ListTableMetadata",
                code: error
                    .as_service_error()
                    .and_then(|service| service.code())
                    .unwrap_or("request failed")
                    .to_owned(),
                message: crate::error::service_message(error.as_service_error()),
            })?;
        Ok(TablePage {
            tables: response
                .table_metadata_list()
                .iter()
                .map(TableMetadata::from)
                .collect(),
            next_token: response.next_token().map(str::to_owned),
        })
    }

    /// Open a table's catalog description. This never runs SELECT or UNLOAD.
    pub async fn table_metadata(&self, table: &str) -> Result<TableMetadata, Error> {
        if table.trim().is_empty() || table.chars().any(char::is_control) {
            return Err(Error::InvalidSetting("table"));
        }
        let response = aws_sdk_athena::Client::new(self.aws_config())
            .get_table_metadata()
            .catalog_name(CATALOG)
            .database_name(&self.options().schema)
            .work_group(&self.options().workgroup)
            .table_name(table)
            .send()
            .await
            .map_err(|error| Error::Service {
                operation: "GetTableMetadata",
                code: error
                    .as_service_error()
                    .and_then(|service| service.code())
                    .unwrap_or("request failed")
                    .to_owned(),
                message: crate::error::service_message(error.as_service_error()),
            })?;
        response
            .table_metadata()
            .map(TableMetadata::from)
            .ok_or(Error::InvalidServiceResponse("GetTableMetadata"))
    }
}

impl From<&aws_sdk_athena::types::Column> for ColumnMetadata {
    fn from(column: &aws_sdk_athena::types::Column) -> Self {
        Self {
            name: column.name().to_owned(),
            data_type: column.r#type().map(str::to_owned),
            comment: column.comment().map(str::to_owned),
        }
    }
}

impl From<&aws_sdk_athena::types::TableMetadata> for TableMetadata {
    fn from(table: &aws_sdk_athena::types::TableMetadata) -> Self {
        Self {
            name: table.name().to_owned(),
            table_type: table.table_type().map(str::to_owned),
            columns: table.columns().iter().map(ColumnMetadata::from).collect(),
            partition_keys: table
                .partition_keys()
                .iter()
                .map(ColumnMetadata::from)
                .collect(),
        }
    }
}
