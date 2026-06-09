# Deployment Guide

This guide covers deploying Hadrian Gateway using Docker and Docker Compose in various configurations.

## Prerequisites

- Docker 20.10+
- Docker Compose 2.0+
- LLM Provider API key (e.g., OpenRouter, OpenAI, Anthropic)

## Quick Start

### 1. Clone and Configure

```bash
# Clone the repository
cd /path/to/hadrian/gateway

# Copy environment variables
cp .env.example .env

# Edit .env and add your API keys
nano .env
```

### 2. Choose Your Deployment

We provide three deployment configurations:

| Configuration          | Use Case             | Components                   |
|------------------------|----------------------|------------------------------|
| **SQLite**             | Development, Testing | Gateway + SQLite             |
| **SQLite + Redis**     | Enhanced Development | Gateway + SQLite + Redis     |
| **PostgreSQL + Redis** | Production           | Gateway + PostgreSQL + Redis |

## Deployment Options

### Option 1: SQLite (Development)

Simplest deployment for development and testing. Data persists in a Docker volume.

```bash
# Start the gateway
docker-compose -f docker-compose.sqlite.yml up -d

# View logs
docker-compose -f docker-compose.sqlite.yml logs -f gateway

# Stop the gateway
docker-compose -f docker-compose.sqlite.yml down
```

**Features:**

- ✅ Single container
- ✅ Persistent SQLite database
- ✅ Fast startup
- ✅ Easy local development
- ❌ No caching
- ❌ Not suitable for production

**Access:**

- API: http://localhost:8080
- Admin API: http://localhost:8080/admin

### Option 2: SQLite + Redis (Enhanced Development)

Development deployment with Redis caching for better performance.

```bash
# Start the gateway and Redis
docker-compose -f docker-compose.sqlite-redis.yml up -d

# View logs
docker-compose -f docker-compose.sqlite-redis.yml logs -f

# Stop everything
docker-compose -f docker-compose.sqlite-redis.yml down
```

**Features:**

- ✅ Redis caching for API keys and usage
- ✅ Better performance than SQLite-only
- ✅ Still simple to run locally
- ✅ Persistent database and cache
- ❌ Not suitable for production scale

**Access:**

- API: http://localhost:8080
- Admin API: http://localhost:8080/admin
- Redis: localhost:6379

### Option 3: PostgreSQL + Redis (Production)

Full production deployment with PostgreSQL and Redis.

```bash
# Set a secure PostgreSQL password in .env
echo "POSTGRES_PASSWORD=$(openssl rand -base64 32)" >> .env

# Start all services
docker-compose -f docker-compose.postgres.yml up -d

# View logs
docker-compose -f docker-compose.postgres.yml logs -f

# Stop everything
docker-compose -f docker-compose.postgres.yml down
```

**Features:**

- ✅ Production-ready database
- ✅ Redis caching
- ✅ High performance
- ✅ Scalable
- ✅ Persistent data
- ✅ Health checks

**Access:**

- API: http://localhost:8080
- Admin API: http://localhost:8080/admin
- PostgreSQL: localhost:5432
- Redis: localhost:6379

## Database Migrations

### SQLite

Migrations are automatically applied on first run. The database file is created at `/app/data/hadrian.db` inside the
container.

To access the database:

```bash
# SQLite deployment
docker exec -it hadrian-gateway sqlite3 /app/data/hadrian.db
```

### PostgreSQL

Migrations are automatically applied on first run via the init scripts.

To access the database:

```bash
# PostgreSQL deployment
docker exec -it hadrian-postgres psql -U gateway -d gateway
```

## Environment Variables

### Required

| Variable             | Description        | Example        |
|----------------------|--------------------|----------------|
| `OPENROUTER_API_KEY` | OpenRouter API key | `sk-or-v1-...` |

### Optional

| Variable            | Description               | Example              | Default        |
|---------------------|---------------------------|----------------------|----------------|
| `ANTHROPIC_API_KEY` | Anthropic API key         | `sk-ant-...`         | -              |
| `OPENAI_API_KEY`    | OpenAI API key            | `sk-...`             | -              |
| `POSTGRES_PASSWORD` | PostgreSQL password       | `secure-password`    | `gateway`      |
| `DATABASE_URL`      | PostgreSQL connection URL | `postgres://...`     | Auto-generated |
| `REDIS_URL`         | Redis connection URL      | `redis://redis:6379` | Auto-generated |

## Creating Your First Organization

Once deployed, create an organization via the Admin API:

```bash
# Create an organization
curl -X POST http://localhost:8080/admin/v1/organizations \
  -H "Content-Type: application/json" \
  -d '{
    "slug": "my-org",
    "name": "My Organization"
  }'

# Create an API key for the organization
curl -X POST http://localhost:8080/admin/v1/api-keys \
  -H "Content-Type: application/json" \
  -d '{
    "name": "Production Key",
    "owner": {
      "type": "organization",
      "org_id": "<org-id-from-above>"
    },
    "budget_limit_cents": 10000,
    "budget_period": "monthly"
  }'
```

Save the API key from the response - it's only shown once!

## Testing the Deployment

Test the gateway with a chat completion request:

```bash
curl -X POST http://localhost:8080/api/v1/chat/completions \
  -H "X-API-Key: gw_live_..." \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-3.5-turbo",
    "messages": [
      {"role": "user", "content": "Hello!"}
    ]
  }'
```

## Monitoring

### View Logs

```bash
# All services
docker-compose -f docker-compose.postgres.yml logs -f

# Just the gateway
docker-compose -f docker-compose.postgres.yml logs -f gateway

# Just PostgreSQL
docker-compose -f docker-compose.postgres.yml logs -f postgres
```

### Health Checks

All services include health checks:

```bash
# Check container health
docker ps

# Gateway health endpoint
curl http://localhost:8080/health
```

### Usage Metrics

Query the database for usage metrics:

```bash
# PostgreSQL
docker exec -it hadrian-postgres psql -U gateway -d gateway -c \
  "SELECT model, COUNT(*) as requests, SUM(cost_cents) as total_cost_cents
   FROM usage_records
   GROUP BY model
   ORDER BY total_cost_cents DESC;"

# SQLite
docker exec -it hadrian-gateway sqlite3 /app/data/hadrian.db \
  "SELECT model, COUNT(*) as requests, SUM(cost_cents) as total_cost_cents
   FROM usage_records
   GROUP BY model
   ORDER BY total_cost_cents DESC;"
```

## Backup and Restore

### SQLite

```bash
# Backup
docker cp hadrian-gateway:/app/data/hadrian.db ./backup.db

# Restore
docker cp ./backup.db hadrian-gateway:/app/data/hadrian.db
docker-compose -f docker-compose.sqlite.yml restart gateway
```

### PostgreSQL

```bash
# Backup
docker exec hadrian-postgres pg_dump -U gateway gateway > backup.sql

# Restore
cat backup.sql | docker exec -i hadrian-postgres psql -U gateway gateway
```

## Scaling

### Horizontal Scaling

To run multiple gateway instances behind a load balancer:

1. Use PostgreSQL + Redis deployment
2. Run multiple gateway containers:

```yaml
services:
  gateway:
    # ... existing config ...
    deploy:
      replicas: 3
```

3. Add a load balancer (nginx, traefik, etc.)

### Vertical Scaling

Adjust resource limits in docker-compose:

```yaml
services:
  gateway:
    # ... existing config ...
    deploy:
      resources:
        limits:
          cpus: '2'
          memory: 2G
        reservations:
          cpus: '1'
          memory: 1G
```

## Troubleshooting

### Gateway won't start

```bash
# Check logs
docker-compose logs gateway

# Common issues:
# 1. Missing API key - check .env file
# 2. Database connection - ensure postgres is healthy
# 3. Port conflict - check if 8080 is available
```

### Database connection errors

```bash
# Check PostgreSQL is running
docker-compose ps postgres

# Check PostgreSQL logs
docker-compose logs postgres

# Test connection
docker exec -it hadrian-postgres psql -U gateway -d gateway -c "SELECT 1;"
```

### Redis connection errors

```bash
# Check Redis is running
docker-compose ps redis

# Test connection
docker exec -it hadrian-redis redis-cli ping
```

## Security Considerations

### Production Checklist

- [ ] Use strong `POSTGRES_PASSWORD`
- [ ] Don't expose database ports (remove `ports:` from postgres service)
- [ ] Don't expose Redis ports (remove `ports:` from redis service)
- [ ] Use HTTPS/TLS (add reverse proxy)
- [ ] Set up firewall rules
- [ ] Enable PostgreSQL SSL
- [ ] Use Redis AUTH (add `--requirepass`)
- [ ] Regular backups
- [ ] Monitor logs for suspicious activity

### Recommended Production Setup

```yaml
# docker-compose.prod.yml
services:
  gateway:
    # ... existing config ...
    environment:
      # Add production settings
      - RUST_LOG=info
    networks:
      - internal
      - external

  postgres:
    # Remove exposed ports
    # ports: - "5432:5432"  # REMOVE THIS
    networks:
      - internal

  redis:
    # Remove exposed ports and add auth
    # ports: - "6379:6379"  # REMOVE THIS
    command: redis-server --appendonly yes --requirepass ${REDIS_PASSWORD}
    networks:
      - internal

  # Add reverse proxy
  nginx:
    image: nginx:alpine
    ports:
      - "80:80"
      - "443:443"
    volumes:
      - ./nginx.conf:/etc/nginx/nginx.conf:ro
      - ./certs:/etc/nginx/certs:ro
    networks:
      - external
    depends_on:
      - gateway

networks:
  internal:
    driver: bridge
  external:
    driver: bridge
```

## Updates

### Updating the Gateway

```bash
# Pull latest changes
git pull

# Rebuild and restart
docker-compose -f docker-compose.postgres.yml build
docker-compose -f docker-compose.postgres.yml up -d

# Check logs
docker-compose -f docker-compose.postgres.yml logs -f gateway
```

### Database Migrations

New versions may include database migrations. The gateway applies migrations automatically on startup.

## Support

For issues and questions:

- GitHub Issues: https://github.com/hadriangateway/hadrian/issues
- Documentation: See README.md
