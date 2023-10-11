POSTGRES_PASSWORD := 'postgres'
DATABASE_URL := 'postgresql://postgres:postgres@0.0.0.0:5431/postgres'

watch-data-plane:
    echo "<<<<<<< START LOCAL DEV SUITE >>>>>>>"; \
    just -f ./tembo-operator/justfile start-kind & \
    just -f ./tembo-operator/justfile watch & \
    just -f ./conductor/justfile watch & \
    just run-dbs

run-control-plane:
    docker run -it --entrypoint /usr/local/bin/cp-webserver --rm quay.io/coredb/cp-service

run-dbs:
	docker rm --force pgmq-pg || true
	docker run -d --name pgmq-pg -e POSTGRES_PASSWORD={{POSTGRES_PASSWORD}} -p 5431:5432 quay.io/tembo/pgmq-pg:v0.14.2


db-cleanup:
	docker stop pgmq-pg
