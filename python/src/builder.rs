use ballista::prelude::SessionContextExt;
use datafusion::prelude::SessionContext;
use datafusion_python::{context::PySessionContext, utils::wait_for_future};
use pyo3::{marker::Python, pyclass, pymethods, PyResult};

#[pyclass(name = "Ballista", module = "pyballista", subclass)]
pub struct Ballista {}

#[pymethods]
impl Ballista {
    #[staticmethod]
    pub fn standalone(py: Python) -> PyResult<PySessionContext> {
        let session_context = SessionContext::standalone();
        let ctx = wait_for_future(py, session_context)?;

        Ok(ctx.into())
    }

    #[staticmethod]
    pub fn remote(host: &str, port: u16, py: Python) -> PyResult<PySessionContext> {
        let session_context = SessionContext::remote(host, port);
        let ctx = wait_for_future(py, session_context)?;

        Ok(ctx.into())
    }
}
