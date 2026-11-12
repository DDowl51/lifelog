//! 契约：执行 SQL。扩展点：各组件登记自己的迁移。
//!
//! 存储服务不认识任何表。表、迁移、SQL 都归用它的组件；
//! 存储服务只负责执行，以及在启动时把登记上来的迁移跑完。

use kernel_api::{AppError, contract};
use serde::{Deserialize, Serialize};

/// 一个 SQL 值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SqlValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl SqlValue {
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(v) => Some(v),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}

impl From<i64> for SqlValue {
    fn from(v: i64) -> Self {
        Self::Integer(v)
    }
}

impl From<i32> for SqlValue {
    fn from(v: i32) -> Self {
        Self::Integer(i64::from(v))
    }
}

impl From<bool> for SqlValue {
    fn from(v: bool) -> Self {
        Self::Integer(i64::from(v))
    }
}

impl From<f64> for SqlValue {
    fn from(v: f64) -> Self {
        Self::Real(v)
    }
}

impl From<String> for SqlValue {
    fn from(v: String) -> Self {
        Self::Text(v)
    }
}

impl From<&str> for SqlValue {
    fn from(v: &str) -> Self {
        Self::Text(v.to_owned())
    }
}

impl From<Vec<u8>> for SqlValue {
    fn from(v: Vec<u8>) -> Self {
        Self::Blob(v)
    }
}

impl<T: Into<SqlValue>> From<Option<T>> for SqlValue {
    fn from(v: Option<T>) -> Self {
        v.map_or(Self::Null, Into::into)
    }
}

/// 一条语句和它的参数。参数用 `?1`、`?2`……按位置引用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Statement {
    pub sql: String,
    pub params: Vec<SqlValue>,
}

impl Statement {
    pub fn new(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            params: Vec::new(),
        }
    }

    /// 追加一个参数。
    #[must_use]
    pub fn bind(mut self, value: impl Into<SqlValue>) -> Self {
        self.params.push(value.into());
        self
    }
}

/// 一条语句的结果。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Rows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<SqlValue>>,
    /// 这条语句改动了多少行。查询为 0。
    pub affected: u64,
}

contract! {
    /// 执行 SQL。
    pub trait Sql as SQL = service("sql", 1) {
        /// 执行一条只读语句。不是只读语句时返回 `validation`。
        async fn query(&self, statement: Statement) -> Result<Rows, AppError>;

        /// 在一个事务里依次执行一批语句，返回每条语句的结果。
        /// 任何一条失败，整批都不生效。
        async fn execute(&self, statements: Vec<Statement>) -> Result<Vec<Rows>, AppError>;
    }
}

/// 一步迁移。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Migration {
    /// 从 1 开始连续递增。
    pub version: u32,
    pub name: String,
    /// 可以包含多条语句。只能创建、修改名字以 `<组件 id>_` 开头的表、索引、视图
    /// （id 里的 `-` 换成 `_`）。
    pub sql: String,
}

/// 一个组件的全部迁移。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationSet {
    /// 组件 id，也是表名前缀的来源。
    pub component: String,
    pub migrations: Vec<Migration>,
}

contract! {
    /// 扩展点：有表的组件来登记自己的迁移。
    ///
    /// 存储服务在全部组件登记完之后执行还没执行过的迁移，并记下每一步的校验和。
    /// **已经执行过的迁移被改动时拒绝启动**：改表结构只能追加新版本。
    pub trait MigrationSource as MIGRATIONS = point("sql.migrations", 1) {
        async fn migrations(&self) -> Result<MigrationSet, AppError>;
    }
}

/// 登记迁移的便利写法：一份固定的迁移清单。不属于契约。
pub struct StaticMigrations(pub MigrationSet);

impl StaticMigrations {
    /// `steps` 是按版本顺序排列的（名字，SQL），版本号从 1 开始自动编号。
    pub fn new(component: &str, steps: &[(&str, &str)]) -> Self {
        let migrations = steps
            .iter()
            .zip(1u32..)
            .map(|((name, sql), version)| Migration {
                version,
                name: (*name).to_owned(),
                sql: (*sql).to_owned(),
            })
            .collect();
        Self(MigrationSet {
            component: component.to_owned(),
            migrations,
        })
    }
}

#[kernel_api::async_trait]
impl MigrationSource for StaticMigrations {
    async fn migrations(&self) -> Result<MigrationSet, AppError> {
        Ok(self.0.clone())
    }
}

#[cfg(feature = "testing")]
pub mod testing {
    //! `Sql` 的契约测试。每个实现都要跑。

    use std::sync::Arc;

    use kernel_api::{AppError, ErrorKind};

    use super::{Rows, Sql, SqlValue, Statement};

    fn st(sql: &str) -> Statement {
        Statement::new(sql)
    }

    async fn count(sql: &Arc<dyn Sql>, table: &str) -> Result<i64, AppError> {
        let rows = sql
            .query(st(&format!("SELECT count(*) FROM {table}")))
            .await?;
        Ok(rows.rows[0][0].as_i64().unwrap_or(-1))
    }

    /// 传进来的实现要是空的、可写的。
    pub async fn sql_contract(sql: Arc<dyn Sql>) -> Result<(), AppError> {
        // 一批语句一个事务；每条语句各有结果
        let results = sql
            .execute(vec![
                st("CREATE TABLE contract_t (id INTEGER PRIMARY KEY, i INTEGER, r REAL, t TEXT, b BLOB)"),
                st("INSERT INTO contract_t (i, r, t, b) VALUES (?1, ?2, ?3, ?4)")
                    .bind(42_i64)
                    .bind(1.5)
                    .bind("中文 ✓")
                    .bind(vec![0_u8, 255]),
                st("INSERT INTO contract_t (i) VALUES (?1) RETURNING id, i").bind(None::<i64>),
            ])
            .await?;
        assert_eq!(results.len(), 3, "每条语句一个结果");
        assert_eq!(results[1].affected, 1);
        assert_eq!(
            results[2],
            Rows {
                columns: vec!["id".to_owned(), "i".to_owned()],
                rows: vec![vec![SqlValue::Integer(2), SqlValue::Null]],
                affected: 1,
            },
            "RETURNING 的行要带回来"
        );

        // 五种值都能原样读回
        let rows = sql
            .query(st("SELECT i, r, t, b, NULL AS n FROM contract_t WHERE id = ?1").bind(1_i64))
            .await?;
        assert_eq!(rows.columns, ["i", "r", "t", "b", "n"]);
        assert_eq!(
            rows.rows,
            [[
                SqlValue::Integer(42),
                SqlValue::Real(1.5),
                SqlValue::Text("中文 ✓".to_owned()),
                SqlValue::Blob(vec![0, 255]),
                SqlValue::Null,
            ]]
        );
        assert_eq!(rows.affected, 0);

        // 一条失败，整批不生效
        let err = sql
            .execute(vec![
                st("INSERT INTO contract_t (i) VALUES (7)"),
                st("INSERT INTO contract_no_such_table (i) VALUES (8)"),
            ])
            .await
            .err();
        assert!(err.is_some(), "第二条语句应该失败");
        assert_eq!(
            count(&sql, "contract_t").await?,
            2,
            "失败的那一批不能留下任何东西"
        );

        // 违反约束是 validation
        let err = sql
            .execute(vec![st("INSERT INTO contract_t (id, i) VALUES (1, 0)")])
            .await
            .err()
            .map(|e| e.kind);
        assert_eq!(err, Some(ErrorKind::Validation), "主键冲突");

        // query 只接受只读语句
        let err = sql
            .query(st("DELETE FROM contract_t"))
            .await
            .err()
            .map(|e| e.kind);
        assert_eq!(err, Some(ErrorKind::Validation), "query 不能写");
        assert_eq!(count(&sql, "contract_t").await?, 2);

        // 参数个数不对是 validation
        let err = sql
            .query(st("SELECT ?1, ?2").bind(1_i64))
            .await
            .err()
            .map(|e| e.kind);
        assert_eq!(err, Some(ErrorKind::Validation), "少给了参数");

        // 语法错误是 validation
        let err = sql.query(st("SELEKT 1")).await.err().map(|e| e.kind);
        assert_eq!(err, Some(ErrorKind::Validation), "语法错误");

        // 空的一批
        assert!(sql.execute(vec![]).await?.is_empty());

        sql.execute(vec![st("DROP TABLE contract_t")]).await?;
        Ok(())
    }
}
