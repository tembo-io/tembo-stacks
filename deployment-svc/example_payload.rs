let deployment = serde_json::json!({
    "apiVersion": "postgres-operator.crunchydata.com/v1beta1",
    "kind": "PostgresCluster",
    "metadata": {
        "name": "dummy",
    },
    "spec": {
        "image": "registry.developers.crunchydata.com/crunchydata/crunchy-postgres:ubi8-14.6-2",
        "postgresVersion": 14,
        "instances": [
            {
                "name": "instance1",
                "dataVolumeClaimSpec": {
                    "accessModes": ["ReadWriteOnce"],
                    "resources": {"requests": {"storage": "1Gi"}},
                },
            },
        ],
        "backups": {
            "pgbackrest": {
                "image": "registry.developers.crunchydata.com/crunchydata/crunchy-pgbackrest:ubi8-2.41-2",
                "repos": [
                    {
                        "name": "repo1",
                        "volume": {
                            "volumeClaimSpec": {
                                "accessModes": ["ReadWriteOnce"],
                                "resources": {"requests": {"storage": "1Gi"}},
                            },
                        },
                    },
                ],
            }
        },
    },
});
