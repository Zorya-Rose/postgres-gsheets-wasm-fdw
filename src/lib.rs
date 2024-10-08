#[allow(warnings)]
mod bindings;
use serde_json::Value as JsonValue;

use bindings::{
    exports::supabase::wrappers::routines::Guest,
    supabase::wrappers::{
        http, time,
        types::{Cell, Context, FdwError, FdwResult, OptionsType, Row, TypeOid},
        utils,
    },
};

#[derive(Debug, Default)]
struct ExampleFdw {
    base_url: String,
    src_rows: Vec<JsonValue>,
    src_idx: usize,
}

// pointer for the static FDW instance
static mut INSTANCE: *mut ExampleFdw = std::ptr::null_mut::<ExampleFdw>();

impl ExampleFdw {
    // initialise FDW instance
    fn init_instance() {
        let instance = Self::default();
        unsafe {
            INSTANCE = Box::leak(Box::new(instance));
        }
    }

    fn this_mut() -> &'static mut Self {
        unsafe { &mut (*INSTANCE) }
    }
}

impl Guest for ExampleFdw {
    fn host_version_requirement() -> String {
        // semver expression for Wasm FDW host version requirement
        // ref: https://docs.rs/semver/latest/semver/enum.Op.html
        "^0.1.0".to_string()
    }

    fn init(ctx: &Context) -> FdwResult {
        Self::init_instance();
        let this = Self::this_mut();
    
        // get API URL from foreign server options if it is specified
        let opts = ctx.get_options(OptionsType::Server);
        this.base_url = opts.require_or("base_url", "https://docs.google.com/spreadsheets/d");
    
        Ok(())
    }

    fn begin_scan(ctx: &Context) -> FdwResult {
        let this = Self::this_mut();
    
        // get sheet id from foreign table options and make the request URL
        let opts = ctx.get_options(OptionsType::Table);
        let sheet_id = opts.require("sheet_id")?;
        // expecting input with a format like "&sheet=posts"
        let sub_sheet_id = opts.require_or("sub_sheet_id", "");
        let url = format!("{}/{}/gviz/tq?tqx=out:json{}", this.base_url, sheet_id, sub_sheet_id);
    
        // make up request headers
        let headers: Vec<(String, String)> = vec![
            ("user-agent".to_owned(), "Sheets FDW".to_owned()),
            // header to make JSON response more cleaner
            ("x-datasource-auth".to_owned(), "true".to_owned()),
        ];
    
        // make a request to Google API and parse response as JSON
        let req = http::Request {
            method: http::Method::Get,
            url,
            headers,
            body: String::default(),
        };
        let resp = http::get(&req)?;
        // remove invalid prefix from response to make a valid JSON string
        let body = resp.body.strip_prefix(")]}'\n").ok_or("invalid response")?;
        let resp_json: JsonValue = serde_json::from_str(body).map_err(|e| e.to_string())?;
    
        // extract source rows from response
        this.src_rows = resp_json
            .pointer("/table/rows")
            .ok_or("cannot get rows from response")
            .map(|v| v.as_array().unwrap().to_owned())?;
    
        // output a Postgres INFO to user (visible in psql), also useful for debugging
        utils::report_info(&format!(
            "We got response array length: {}",
            this.src_rows.len()
        ));
    
        Ok(())
    }

    fn iter_scan(ctx: &Context, row: &Row) -> Result<Option<u32>, FdwError> {
        let this = Self::this_mut();
    
        // if all source rows are consumed, stop data scan
        if this.src_idx >= this.src_rows.len() {
            return Ok(None);
        }
    
        // extract current source row, an example of the source row in JSON:
        // {
        //   "c": [{
        //      "v": 1.0,
        //      "f": "1"
        //    }, {
        //      "v": "Erlich Bachman"
        //    }, null, null, null, null, { "v": null }
        //    ]
        // }
        let src_row = &this.src_rows[this.src_idx];
    
        // loop through each target column, map source cell to target cell
        for tgt_col in ctx.get_columns() {
            let (tgt_col_num, tgt_col_name) = (tgt_col.num(), tgt_col.name());
            if let Some(src) = src_row.pointer(&format!("/c/{}/v", tgt_col_num - 1)) {

                // TypeOid::Bool -> Cell::Bool
                // TypeOid::I8 -> Cell::I8
                // TypeOid::I16 -> Cell::I16
                // TypeOid::F32 -> Cell::F32
                // TypeOid::I32 -> Cell::I32
                // TypeOid::F64 -> Cell::F64
                // TypeOid::I64 -> Cell::I64
                // TypeOid::Numeric -> Cell::Numeric
                // TypeOid::String -> Cell::String
                // TypeOid::Date -> Cell::Date
                // TypeOid::Timestamp -> Cell::Timestamp
                // TypeOid::Timestamptz -> Cell::Timestamptz

                let cell = match tgt_col.type_oid() {
                    TypeOid::Bool => src.as_bool().map(Cell::Bool),
                    TypeOid::I8 => src.as_i64().map(Cell::I64),
                    TypeOid::I16 => src.as_i64().map(Cell::I64),
                    TypeOid::F32 => src.as_f64().map(Cell::F64),
                    TypeOid::I32 => src.as_i64().map(Cell::I64),
                    TypeOid::F64 => src.as_f64().map(Cell::F64),
                    TypeOid::I64 => src.as_i64().map(Cell::I64),
                    TypeOid::Numeric => src.as_f64().map(Cell::Numeric),
                    TypeOid::String => src.as_str().map(|v| Cell::String(v.to_owned())),
                    TypeOid::Date => {
                        if let Some(s) = src.as_str() {
                            let ts = time::parse_from_rfc3339(s)?;
                            Some(Cell::Date(ts / 1_000_000))
                        } else {
                            None
                        }
                    }
                    TypeOid::Timestamp => {
                        if let Some(s) = src.as_str() {
                            let ts = time::parse_from_rfc3339(s)?;
                            Some(Cell::Timestamp(ts))
                        } else {
                            None
                        }
                    }
                    TypeOid::Timestamptz => {
                        if let Some(s) = src.as_str() {
                            let ts = time::parse_from_rfc3339(s)?;
                            Some(Cell::Timestamptz(ts))
                        } else {
                            None
                        }
                    }
                    TypeOid::Json => src.as_object().map(|_| Cell::Json(src.to_string())),
                };
    
                // push the cell to target row
                row.push(cell.as_ref());
            } else {
                row.push(None);
            }
        }
    
        // advance to next source row
        this.src_idx += 1;
    
        // tell Postgres we've done one row, and need to scan the next row
        Ok(Some(0))
    }

    fn re_scan(_ctx: &Context) -> FdwResult {
        Err("re_scan on foreign table is not supported".to_owned())
    }

    fn end_scan(_ctx: &Context) -> FdwResult {
        let this = Self::this_mut();
        this.src_rows.clear();
        Ok(())
    }

    fn begin_modify(_ctx: &Context) -> FdwResult {
        Err("modify on foreign table is not supported".to_owned())
    }

    fn insert(_ctx: &Context, _row: &Row) -> FdwResult {
        Ok(())
    }

    fn update(_ctx: &Context, _rowid: Cell, _row: &Row) -> FdwResult {
        Ok(())
    }

    fn delete(_ctx: &Context, _rowid: Cell) -> FdwResult {
        Ok(())
    }

    fn end_modify(_ctx: &Context) -> FdwResult {
        Ok(())
    }
}

bindings::export!(ExampleFdw with_types_in bindings);
