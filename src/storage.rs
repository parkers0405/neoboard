//! Synchronous boundary for the serialized board writer. Local Turso futures drive
//! embedded I/O themselves; polling them does not enter a nested Tokio runtime.
use futures_executor::block_on;
use std::{ops::Deref, path::Path, time::Duration};
use turso::{Error, Result, Value};

pub struct Connection {
    inner: turso::Connection,
    rollback_error: Option<Error>,
}

impl Connection {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_str().ok_or_else(|| Error::Misuse("database path must be UTF-8".into()))?;
        // The Windows I/O backend does not implement multiprocess WAL. The
        // daemon holds service.lock exclusively before opening the board.
        let db = block_on(turso::Builder::new_local(path).experimental_multiprocess_wal(cfg!(unix) && path != ":memory:").build())?;
        Ok(Self { inner: db.connect()?, rollback_error: None })
    }
    fn ready(&self) -> Result<()> {
        match &self.rollback_error {
            Some(e) => Err(Error::Error(format!("connection unusable after failed rollback: {e}"))),
            None => Ok(()),
        }
    }
    pub fn busy_timeout(&self, duration: Duration) -> Result<()> { self.inner.busy_timeout(duration) }
    pub fn last_insert_rowid(&self) -> i64 { self.inner.last_insert_rowid() }
    pub fn execute(&self, sql: &str, params: impl Bindings) -> Result<u64> {
        self.ready()?;
        block_on(self.inner.execute(sql, params.values()?))
    }
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        self.ready()?;
        block_on(self.inner.execute_batch(sql))
    }
    pub fn prepare(&self, sql: &str) -> Result<Statement> {
        self.ready()?;
        Ok(Statement(block_on(self.inner.prepare(sql))?))
    }
    pub fn query_row<T>(&self, sql: &str, params: impl Bindings, map: impl FnOnce(&Row) -> Result<T>) -> Result<T> {
        let mut rows = self.prepare(sql)?.query(params)?;
        map(&rows.next()?.ok_or(Error::QueryReturnedNoRows)?)
    }
    pub fn transaction(&mut self) -> Result<Transaction<'_>> {
        self.execute_batch("BEGIN IMMEDIATE")?;
        Ok(Transaction { conn: self, committed: false })
    }
}

pub struct Transaction<'a> { conn: &'a mut Connection, committed: bool }
impl Deref for Transaction<'_> { type Target = Connection; fn deref(&self) -> &Connection { self.conn } }
impl Transaction<'_> {
    pub fn commit(mut self) -> Result<()> {
        self.execute_batch("COMMIT")?;
        self.committed = true;
        Ok(())
    }
}
impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.committed && !matches!(self.conn.inner.is_autocommit(), Ok(true)) {
            self.conn.rollback_error = self.conn.execute_batch("ROLLBACK").err();
        }
    }
}

pub struct Statement(turso::Statement);
pub struct Rows(turso::Rows);
pub struct Row(turso::Row);
impl Statement {
    pub fn query(&mut self, params: impl Bindings) -> Result<Rows> {
        Ok(Rows(block_on(self.0.query(params.values()?))?))
    }
    pub fn query_map<T>(&mut self, params: impl Bindings, mut map: impl FnMut(&Row) -> Result<T>) -> Result<impl Iterator<Item=Result<T>>> {
        let mut rows = self.query(params)?;
        Ok(std::iter::from_fn(move || rows.next().transpose().map(|r| r.and_then(|row| map(&row)))))
    }
}
impl Rows { pub fn next(&mut self) -> Result<Option<Row>> { Ok(block_on(self.0.next())?.map(Row)) } }
impl Row {
    pub fn get<I: TryInto<usize>, T: Column>(&self, i: I) -> Result<T> { T::decode(self.0.get_value(i.try_into().map_err(|_| mismatch())?)?) }
}
pub trait Column: Sized { fn decode(v: Value) -> Result<Self>; }
fn mismatch() -> Error { Error::ConversionFailure("unexpected SQL column type or range".into()) }
macro_rules! column { ($t:ty, $pat:pat => $v:expr) => {impl Column for $t {fn decode(v: Value) -> Result<Self> {match v {$pat => $v, _ => Err(mismatch())}}}}; }
column!(String, Value::Text(s) => Ok(s));
column!(Vec<u8>, Value::Blob(b) => Ok(b));
column!(bool, Value::Integer(i) => Ok(i != 0));
macro_rules! integer { ($($t:ty),*) => {$(column!($t, Value::Integer(i) => i.try_into().map_err(|_| mismatch()));)*}; }
integer!(u64,u32,i64,usize,i32);
impl<T: Column> Column for Option<T> { fn decode(v: Value) -> Result<Self> { if v.is_null() { Ok(None) } else { T::decode(v).map(Some) } } }

pub trait OptionalExtension<T> { fn optional(self) -> Result<Option<T>>; }
impl<T> OptionalExtension<T> for Result<T> {
    fn optional(self) -> Result<Option<T>> { match self { Err(Error::QueryReturnedNoRows) => Ok(None), r => r.map(Some) } }
}

pub trait Bind { fn bind(&self) -> Result<Value>; }
impl<T: Bind + ?Sized> Bind for &T { fn bind(&self) -> Result<Value> { (*self).bind() } }
impl Bind for str { fn bind(&self) -> Result<Value> { Ok(self.into()) } }
impl Bind for String { fn bind(&self) -> Result<Value> { self.as_str().bind() } }
impl Bind for [u8] { fn bind(&self) -> Result<Value> { Ok(self.into()) } }
impl Bind for Vec<u8> { fn bind(&self) -> Result<Value> { self.as_slice().bind() } }
impl Bind for u64 { fn bind(&self) -> Result<Value> { Value::try_from(*self) } }
macro_rules! bind_copy { ($($t:ty),*) => {$(impl Bind for $t {fn bind(&self) -> Result<Value> {Ok((*self).into())}})*}; }
bind_copy!(bool,i32,i64);
impl<T: Bind> Bind for Option<T> { fn bind(&self) -> Result<Value> { self.as_ref().map_or(Ok(Value::Null), Bind::bind) } }
pub trait Bindings { fn values(self) -> Result<Vec<Value>>; }
impl Bindings for [();0] { fn values(self) -> Result<Vec<Value>> { Ok(vec![]) } }
macro_rules! array { ($($n:literal),*) => {$(impl<T: Bind> Bindings for [T;$n] {fn values(self) -> Result<Vec<Value>> {self.iter().map(Bind::bind).collect()}})*}; }
array!(1,2,3);
impl Bindings for Vec<Result<Value>> { fn values(self) -> Result<Vec<Value>> { self.into_iter().collect() } }
#[macro_export]
macro_rules! params { ($($v:expr),+ $(,)?) => { vec![$($crate::storage::Bind::bind(&$v)),+] }; }