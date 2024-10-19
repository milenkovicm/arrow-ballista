# %%
from datafusion.context import SessionContext
from pyballista import Ballista

ctx : SessionContext = Ballista.standalone()
df = ctx.sql("SELECT 1")
batches = df.collect()

batches
# %%
df = ctx.sql("SHOW TABLES").show()
# %%
ctx.register_parquet("p", "testdata/test.parquet")

# %%
ctx.sql("select * from p").show()

# %%
ctx.sql("select name, value from information_schema.df_settings where name like 'ballista%'").show()
# %%
