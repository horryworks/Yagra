-- 0148_yunshan_model_from_last_line - forget the software family stored as a YunShan device's
-- model, so the identity probe stores the real one (ADR-147 Inc.7).
--
-- reversible: data only - it clears one column on some rows and changes no shape. An older core
-- ignores the version it does not embed (every release from 0.2.2 on, as 0108 records) and, rolled
-- back to, refills the cleared value with the old guess; rolling forward again clears it again.
--
-- WHY
-- A YunShan OS sysDescr names its software family in parentheses on the second line
-- ("(S5700 V600R022C01SPC500)") and the device's model on the last ("HUAWEI CloudEngine
-- S5735-L-V2"). `yagra_discovery::identify` took the first model-shaped token, so it stored the
-- family. It now reads the last line, but the fill writes only into an empty column (ADR-147
-- decision 27), so what is already stored would never change on its own.
--
-- WHICH ROWS
-- Only a row whose model is exactly the token the old rule took: the word in the parentheses before
-- the release. A model an operator typed is anything else and is left alone. The column is cleared
-- rather than rewritten so the rule that reads the last line exists once, in Rust; the hourly
-- identity probe (ADR-138) fills it within the hour.
UPDATE nodes
   SET model = NULL
 WHERE sys_descr LIKE '%YunShan OS%'
   AND model IS NOT NULL
   AND model = upper(substring(sys_descr FROM '\(([A-Za-z0-9-]+) V[0-9]'));
