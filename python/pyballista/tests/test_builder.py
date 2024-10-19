from datafusion.context import SessionContext
from pyballista import Ballista
import pytest

def test_create_context():
    ctx: SessionContext = Ballista.standalone()

def test_select_one():
    ctx = Ballista.standalone()
    df = ctx.sql("SELECT 1")
    batches = df.collect()
    assert len(batches) == 1