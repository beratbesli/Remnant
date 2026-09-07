import os

import psycopg
import redis


database_url = os.environ["FIXTURE_DATABASE_URL"]
redis_url = os.environ["FIXTURE_REDIS_URL"]

with psycopg.connect(database_url) as connection:
    with connection.cursor() as cursor:
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS users (
                id integer PRIMARY KEY,
                name text NOT NULL
            );
            CREATE TABLE IF NOT EXISTS subscriptions (
                id integer PRIMARY KEY,
                user_id integer NOT NULL,
                plan text NOT NULL,
                status text NOT NULL
            );
            TRUNCATE TABLE subscriptions, users;
            """
        )
        cursor.executemany(
            "INSERT INTO users (id, name) VALUES (%s, %s)",
            [(user_id, f"noise-user-{user_id}") for user_id in range(1, 101)],
        )
        cursor.execute(
            "INSERT INTO users (id, name) VALUES (%s, %s)",
            (174, "checkout-target"),
        )
        cursor.executemany(
            "INSERT INTO subscriptions (id, user_id, plan, status) VALUES (%s, %s, %s, %s)",
            [
                (subscription_id, subscription_id, "basic", "active")
                for subscription_id in range(1001, 1051)
            ]
            + [(991, 174, "pro", "active")],
        )

cache = redis.Redis.from_url(redis_url, decode_responses=True)
cache.flushdb()
cache.set("user:174:plan", "legacy")
for key_number in range(100):
    cache.set(f"noise:{key_number}", f"value-{key_number}")

print("seeded 101 users, 51 subscriptions, and 101 Redis keys")
