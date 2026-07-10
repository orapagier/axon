-- The standalone Classifier node was consolidated into the Text Analysis node
-- (`textAnalysis`, operation = classify | extract | summarize | sentiment).
-- Rename saved nodes so the editor renders them and the engine dispatches
-- them; the config keys are unchanged (classifier::execute reads the same
-- fields), only the type and the routing `operation` are new. The engine also
-- keeps a `"classifier"` alias arm for snapshots this UPDATE can't reach
-- (workflow_versions restores, imported bundles).
UPDATE workflow_nodes
SET node_type = 'textAnalysis',
    config = CASE
        WHEN json_valid(config) THEN json_set(config, '$.operation', 'classify')
        ELSE config
    END
WHERE node_type = 'classifier';
