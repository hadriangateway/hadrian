#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.12"
# dependencies = [
#     "httpx>=0.27",
#     "beautifulsoup4>=4.12",
#     "rich>=13.7",
# ]
# ///
"""
Knowledge Base Test Script

Fetches recent documents from various public sources and creates vector stores
to test the Knowledge Base / RAG functionality.

Sources:
  - Simon Willison's blog (AI, Python, web development)
  - IETF RFCs (internet standards)
  - arXiv (academic papers)
  - AustLII (Australian legal information)

Usage:
    ./scripts/test-knowledge-bases.py                    # Run with defaults
    ./scripts/test-knowledge-bases.py --source rfc       # Test single source
    ./scripts/test-knowledge-bases.py --gateway-url http://localhost:8080
    ./scripts/test-knowledge-bases.py --dry-run          # Fetch only, don't upload
    ./scripts/test-knowledge-bases.py --list-sources     # Show available sources
    ./scripts/test-knowledge-bases.py --max-file-size 5  # Skip files larger than 5 MB

Environment variables:
    HADRIAN_API_KEY  - API key for authentication (default: test-key)
    HADRIAN_ORG_ID   - Organization ID for ownership (default: test org)
    GATEWAY_URL      - Gateway URL (default: http://localhost:8080)
"""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import sys
import tempfile
import time
import xml.etree.ElementTree as ET
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any
from urllib.parse import urljoin, urlparse

import httpx
from bs4 import BeautifulSoup
from rich.console import Console
from rich.progress import Progress, SpinnerColumn, TextColumn
from rich.table import Table

console = Console()


# =============================================================================
# Configuration
# =============================================================================


@dataclass
class Config:
    """Script configuration."""
    gateway_url: str = "http://localhost:8080"
    api_key: str = "test-key"
    org_id: str = "00000000-0000-0000-0000-000000000001"
    dry_run: bool = False
    max_docs_per_source: int = 3
    timeout: float = 30.0
    embedding_model: str = "text-embedding-3-small"
    max_file_size_mb: float = 10.0

    @classmethod
    def from_env(cls) -> Config:
        return cls(
            gateway_url=os.environ.get("GATEWAY_URL", "http://localhost:8080"),
            api_key=os.environ.get("HADRIAN_API_KEY", "test-key"),
            org_id=os.environ.get("HADRIAN_ORG_ID", "00000000-0000-0000-0000-000000000001"),
        )


@dataclass
class Document:
    """A document fetched from a source."""
    title: str
    content: bytes
    filename: str
    content_type: str
    source: str
    source_url: str
    metadata: dict[str, str] = field(default_factory=dict)


@dataclass
class FetchResult:
    """Result of fetching documents from a source."""
    source_name: str
    documents: list[Document] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)


# =============================================================================
# HTTP Client
# =============================================================================


def create_client(timeout: float = 30.0) -> httpx.Client:
    """Create HTTP client with sensible defaults."""
    return httpx.Client(
        timeout=timeout,
        follow_redirects=True,
        headers={
            "User-Agent": "Hadrian-Gateway-Test/1.0 (https://github.com/hadriangateway/hadrian)",
        },
    )


# =============================================================================
# Document Sources
# =============================================================================


class DocumentSource(ABC):
    """Base class for document sources."""

    @property
    @abstractmethod
    def name(self) -> str:
        """Source name for display."""
        ...

    @property
    @abstractmethod
    def description(self) -> str:
        """Source description."""
        ...

    @abstractmethod
    def fetch(self, client: httpx.Client, max_docs: int) -> FetchResult:
        """Fetch documents from the source."""
        ...


class SimonWillisonSource(DocumentSource):
    """Simon Willison's blog - AI, Python, and web development."""

    FEED_URL = "https://simonwillison.net/atom/everything/"

    @property
    def name(self) -> str:
        return "simonw"

    @property
    def description(self) -> str:
        return "Simon Willison's blog (AI, Python, web development)"

    def fetch(self, client: httpx.Client, max_docs: int) -> FetchResult:
        result = FetchResult(source_name=self.name)

        try:
            response = client.get(self.FEED_URL)
            response.raise_for_status()

            # Parse Atom feed
            root = ET.fromstring(response.content)
            ns = {"atom": "http://www.w3.org/2005/Atom"}

            for entry in root.findall("atom:entry", ns)[:max_docs]:
                try:
                    title_elem = entry.find("atom:title", ns)
                    title = title_elem.text.strip() if title_elem is not None and title_elem.text else "Untitled"

                    # Get the content or summary
                    content_elem = entry.find("atom:content", ns)
                    if content_elem is None:
                        content_elem = entry.find("atom:summary", ns)

                    if content_elem is not None and content_elem.text:
                        html_content = content_elem.text
                        # Extract text from HTML
                        soup = BeautifulSoup(html_content, "html.parser")
                        text_content = soup.get_text(separator="\n", strip=True)
                    else:
                        text_content = ""

                    # Get the link
                    link_elem = entry.find("atom:link[@rel='alternate']", ns)
                    if link_elem is None:
                        link_elem = entry.find("atom:link", ns)
                    url = link_elem.get("href", "") if link_elem is not None else ""

                    # Get published date
                    published_elem = entry.find("atom:published", ns)
                    published = published_elem.text if published_elem is not None else ""

                    if text_content and len(text_content) > 100:
                        # Create a slug from title
                        slug = re.sub(r"[^\w\s-]", "", title.lower())
                        slug = re.sub(r"[-\s]+", "-", slug).strip("-")[:50]

                        result.documents.append(Document(
                            title=title,
                            content=text_content.encode("utf-8"),
                            filename=f"{slug}.txt",
                            content_type="text/plain",
                            source=self.name,
                            source_url=url,
                            metadata={"published": published, "author": "Simon Willison"},
                        ))

                except Exception as e:
                    result.errors.append(f"Failed to parse entry: {e}")

        except httpx.HTTPError as e:
            result.errors.append(f"Failed to fetch feed: {e}")
        except ET.ParseError as e:
            result.errors.append(f"Failed to parse Atom feed: {e}")

        return result


class RFCSource(DocumentSource):
    """IETF RFC documents."""

    # Use IETF's official URLs which are more reliable
    RFC_URLS = [
        "https://www.ietf.org/rfc/rfc{num}.txt",
        "https://datatracker.ietf.org/doc/html/rfc{num}",
    ]

    @property
    def name(self) -> str:
        return "rfc"

    @property
    def description(self) -> str:
        return "IETF RFC (Request for Comments) documents"

    def fetch(self, client: httpx.Client, max_docs: int) -> FetchResult:
        result = FetchResult(source_name=self.name)

        # Fetch some well-known RFCs directly
        # These are selected for their relevance and reasonable size
        recent_rfcs = [
            ("8259", "JSON"),  # Smaller, more likely to succeed
            ("7231", "HTTP/1.1 Semantics"),
            ("7230", "HTTP/1.1 Message Syntax"),
            ("6749", "OAuth 2.0"),
            ("7519", "JSON Web Token (JWT)"),
            ("7540", "HTTP/2"),
            ("8446", "TLS 1.3"),
            ("9110", "HTTP Semantics"),
            ("9111", "HTTP Caching"),
            ("9112", "HTTP/1.1"),
        ]

        for rfc_num, title in recent_rfcs[:max_docs]:
            fetched = False

            # Try multiple URL patterns
            for url_template in self.RFC_URLS:
                if fetched:
                    break

                try:
                    url = url_template.format(num=rfc_num)
                    response = client.get(url)
                    response.raise_for_status()

                    content = response.content
                    content_type = response.headers.get("content-type", "text/plain")

                    # If it's HTML, extract text
                    if "html" in content_type.lower():
                        soup = BeautifulSoup(response.text, "html.parser")
                        # Remove script and style elements
                        for tag in soup(["script", "style", "nav", "header", "footer"]):
                            tag.decompose()
                        text = soup.get_text(separator="\n", strip=True)
                        content = text.encode("utf-8")
                        content_type = "text/plain"

                    result.documents.append(Document(
                        title=f"RFC {rfc_num}: {title}",
                        content=content,
                        filename=f"rfc{rfc_num}.txt",
                        content_type=content_type.split(";")[0],
                        source=self.name,
                        source_url=url,
                        metadata={"rfc_number": rfc_num, "category": "standards"},
                    ))
                    fetched = True

                except httpx.HTTPError:
                    continue

            if not fetched:
                result.errors.append(f"Failed to fetch RFC {rfc_num} from all sources")

        return result


class ArxivSource(DocumentSource):
    """arXiv preprint papers."""

    API_URL = "https://export.arxiv.org/api/query"

    @property
    def name(self) -> str:
        return "arxiv"

    @property
    def description(self) -> str:
        return "arXiv preprint papers (computer science)"

    def fetch(self, client: httpx.Client, max_docs: int) -> FetchResult:
        result = FetchResult(source_name=self.name)

        try:
            # Search for recent CS papers
            params = {
                "search_query": "cat:cs.AI OR cat:cs.LG OR cat:cs.CL",  # AI, ML, NLP
                "sortBy": "submittedDate",
                "sortOrder": "descending",
                "max_results": str(max_docs),
            }

            response = client.get(self.API_URL, params=params)
            response.raise_for_status()

            # Parse Atom feed
            root = ET.fromstring(response.content)
            ns = {"atom": "http://www.w3.org/2005/Atom"}

            for entry in root.findall("atom:entry", ns)[:max_docs]:
                try:
                    title_elem = entry.find("atom:title", ns)
                    title = title_elem.text.strip() if title_elem is not None else "Untitled"
                    title = " ".join(title.split())  # Normalize whitespace

                    # Get the abstract
                    summary_elem = entry.find("atom:summary", ns)
                    abstract = summary_elem.text.strip() if summary_elem is not None else ""
                    abstract = " ".join(abstract.split())

                    # Get the ID (e.g., 2401.12345v1)
                    id_elem = entry.find("atom:id", ns)
                    if id_elem is not None:
                        arxiv_id = id_elem.text.split("/abs/")[-1]
                    else:
                        continue

                    # Get authors
                    authors = []
                    for author in entry.findall("atom:author", ns):
                        name_elem = author.find("atom:name", ns)
                        if name_elem is not None:
                            authors.append(name_elem.text)

                    # Get categories
                    categories = []
                    for cat in entry.findall("atom:category", ns):
                        term = cat.get("term")
                        if term:
                            categories.append(term)

                    # Fetch PDF
                    pdf_url = f"https://arxiv.org/pdf/{arxiv_id}.pdf"
                    try:
                        pdf_response = client.get(pdf_url)
                        pdf_response.raise_for_status()

                        result.documents.append(Document(
                            title=title,
                            content=pdf_response.content,
                            filename=f"arxiv_{arxiv_id.replace('/', '_')}.pdf",
                            content_type="application/pdf",
                            source=self.name,
                            source_url=pdf_url,
                            metadata={
                                "arxiv_id": arxiv_id,
                                "authors": ", ".join(authors[:5]),
                                "categories": ", ".join(categories[:3]),
                                "abstract": abstract[:500],
                            },
                        ))

                    except httpx.HTTPError as e:
                        result.errors.append(f"Failed to fetch PDF for {arxiv_id}: {e}")

                except Exception as e:
                    result.errors.append(f"Failed to parse arXiv entry: {e}")

        except httpx.HTTPError as e:
            result.errors.append(f"Failed to query arXiv API: {e}")
        except ET.ParseError as e:
            result.errors.append(f"Failed to parse arXiv response: {e}")

        return result


class AustLIISource(DocumentSource):
    """Australian Legal Information Institute cases and legislation."""

    BASE_URL = "https://www.austlii.edu.au"

    @property
    def name(self) -> str:
        return "austlii"

    @property
    def description(self) -> str:
        return "Australian legal cases and legislation (AustLII)"

    def fetch(self, client: httpx.Client, max_docs: int) -> FetchResult:
        result = FetchResult(source_name=self.name)

        # Fetch from specific court databases (recent decisions)
        courts = [
            ("/au/cases/cth/HCA/", "High Court of Australia"),
            ("/au/cases/cth/FCA/", "Federal Court of Australia"),
            ("/au/cases/nsw/NSWSC/", "NSW Supreme Court"),
        ]

        docs_fetched = 0
        for court_path, court_name in courts:
            if docs_fetched >= max_docs:
                break

            try:
                # Get the index page for recent cases
                index_url = f"{self.BASE_URL}{court_path}"
                response = client.get(index_url)
                response.raise_for_status()

                soup = BeautifulSoup(response.text, "html.parser")

                # Find links to case decisions
                for link in soup.find_all("a", href=True):
                    if docs_fetched >= max_docs:
                        break

                    href = link["href"]
                    # Look for case links (e.g., /au/cases/cth/HCA/2024/1.html)
                    if re.match(r".*/\d{4}/\d+\.html$", href):
                        case_url = urljoin(self.BASE_URL, href)

                        try:
                            case_response = client.get(case_url)
                            case_response.raise_for_status()

                            case_soup = BeautifulSoup(case_response.text, "html.parser")

                            # Extract case title
                            title_elem = case_soup.find("title")
                            title = title_elem.text.strip() if title_elem else "Untitled Case"

                            # Extract case text
                            content_div = case_soup.find("div", id="content") or case_soup.find("body")
                            if content_div:
                                text_content = content_div.get_text(separator="\n", strip=True)

                                # Generate filename from URL
                                filename = href.replace("/", "_").strip("_")
                                if not filename.endswith(".txt"):
                                    filename = filename.replace(".html", ".txt")

                                result.documents.append(Document(
                                    title=title,
                                    content=text_content.encode("utf-8"),
                                    filename=filename,
                                    content_type="text/plain",
                                    source=self.name,
                                    source_url=case_url,
                                    metadata={
                                        "court": court_name,
                                        "jurisdiction": "Australia",
                                    },
                                ))
                                docs_fetched += 1

                        except httpx.HTTPError as e:
                            result.errors.append(f"Failed to fetch {case_url}: {e}")

            except httpx.HTTPError as e:
                result.errors.append(f"Failed to fetch {court_name} index: {e}")

        return result


# =============================================================================
# Vector Store Operations
# =============================================================================


class VectorStoreClient:
    """Client for Hadrian vector store API."""

    def __init__(self, config: Config):
        self.config = config
        self.client = httpx.Client(
            timeout=config.timeout,
            headers={
                "X-API-Key": config.api_key,
            },
        )

    def upload_file(self, doc: Document) -> dict[str, Any] | None:
        """Upload a file to the gateway."""
        url = f"{self.config.gateway_url}/api/v1/files"

        with tempfile.NamedTemporaryFile(suffix=Path(doc.filename).suffix, delete=False) as f:
            f.write(doc.content)
            temp_path = f.name

        try:
            with open(temp_path, "rb") as f:
                files = {"file": (doc.filename, f, doc.content_type)}
                data = {
                    "purpose": "assistants",
                    "owner_type": "organization",
                    "owner_id": self.config.org_id,
                }
                response = self.client.post(
                    url,
                    files=files,
                    data=data,
                )
                response.raise_for_status()
                return response.json()

        except httpx.HTTPError as e:
            console.print(f"[red]Failed to upload {doc.filename}: {e}[/red]")
            console.print(f"{response.text=}")
            return None
        finally:
            Path(temp_path).unlink(missing_ok=True)

    def find_vector_store(self, name: str) -> dict[str, Any] | None:
        """Find an existing vector store by name."""
        url = f"{self.config.gateway_url}/api/v1/vector_stores"
        params = {
            "owner_type": "organization",
            "owner_id": self.config.org_id,
            "limit": 100,
        }

        try:
            response = self.client.get(url, params=params)
            response.raise_for_status()
            data = response.json()

            for store in data.get("data", []):
                if store.get("name") == name:
                    return store
            return None

        except httpx.HTTPError:
            return None

    def get_or_create_vector_store(self, name: str, description: str) -> dict[str, Any] | None:
        """Get existing vector store or create a new one."""
        # First check if it already exists
        existing = self.find_vector_store(name)
        if existing:
            console.print(f"  [yellow]Using existing vector store: {existing['id']}[/yellow]")
            return existing

        # Create new one
        url = f"{self.config.gateway_url}/api/v1/vector_stores"

        payload = {
            "owner": {
                "type": "organization",
                "organization_id": self.config.org_id,
            },
            "name": name,
            "description": description,
            "embedding_model": self.config.embedding_model,
            "metadata": {
                "created_by": "test-knowledge-bases.py",
                "created_at": datetime.now().isoformat(),
            },
        }

        try:
            response = self.client.post(url, json=payload)
            response.raise_for_status()
            return response.json()
        except httpx.HTTPError as e:
            console.print(f"[red]Failed to create vector store: {e}[/red]")
            console.print(f"{response.text=}")
            return None

    def add_file_to_store(self, vector_store_id: str, file_id: str) -> dict[str, Any] | None:
        """Add a file to a vector store."""
        url = f"{self.config.gateway_url}/api/v1/vector_stores/{vector_store_id}/files"

        # Extract raw UUID from prefixed ID
        raw_file_id = file_id.replace("file-", "")

        payload = {"file_id": raw_file_id}

        try:
            response = self.client.post(url, json=payload)
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            if e.response.status_code == 409:
                # File already in store or other conflict - try to get existing
                console.print(f"  [yellow]File already in store or conflict, skipping[/yellow]")
                return {"id": file_id, "status": "exists"}
            console.print(f"[red]Failed to add file to vector store: {e}[/red]")
            console.print(f"{response.text}")
            return None
        except httpx.HTTPError as e:
            console.print(f"[red]Failed to add file to vector store: {e}[/red]")
            return None

    def wait_for_processing(
        self, vector_store_id: str, file_id: str, max_wait: int = 300
    ) -> bool:
        """Wait for a file to finish processing."""
        url = f"{self.config.gateway_url}/api/v1/vector_stores/{vector_store_id}/files/{file_id}"

        start = time.time()
        while time.time() - start < max_wait:
            try:
                response = self.client.get(url)
                response.raise_for_status()
                data = response.json()

                status = data.get("status", "")
                if status == "completed":
                    return True
                elif status == "failed":
                    error = data.get("last_error", {})
                    console.print(f"[red]Processing failed: {error}[/red]")
                    return False

                time.sleep(2)

            except httpx.HTTPError as e:
                console.print(f"[red]Failed to check status: {e}[/red]")
                console.print(f"{response.text=}")
                return False

        console.print("[yellow]Processing timeout[/yellow]")
        return False

    def search(self, vector_store_id: str, query: str, max_results: int = 5) -> dict[str, Any] | None:
        """Search a vector store."""
        url = f"{self.config.gateway_url}/api/v1/vector_stores/{vector_store_id}/search"

        payload = {
            "query": query,
            "max_num_results": max_results,
        }

        try:
            response = self.client.post(url, json=payload)
            response.raise_for_status()
            return response.json()
        except httpx.HTTPError as e:
            console.print(f"[red]Search failed: {e}[/red]")
            console.print(f"{response.text=}")
            return None

    def delete_vector_store(self, vector_store_id: str) -> bool:
        """Delete a vector store."""
        url = f"{self.config.gateway_url}/api/v1/vector_stores/{vector_store_id}"

        try:
            response = self.client.delete(url)
            response.raise_for_status()
            return True
        except httpx.HTTPError:
            console.print(f"{response.text=}")
            return False

    def delete_file(self, file_id: str) -> bool:
        """Delete a file."""
        url = f"{self.config.gateway_url}/api/v1/files/{file_id}"

        try:
            response = self.client.delete(url)
            response.raise_for_status()
            return True
        except httpx.HTTPError:
            console.print(f"{response.text=}")
            return False


# =============================================================================
# Test Runner
# =============================================================================


SOURCES: dict[str, DocumentSource] = {
    "simonw": SimonWillisonSource(),
    "rfc": RFCSource(),
    "arxiv": ArxivSource(),
    "austlii": AustLIISource(),
}

# Sample queries for each source
SAMPLE_QUERIES: dict[str, list[str]] = {
    "simonw": [
        "LLM prompt engineering",
        "SQLite database",
        "Python async",
    ],
    "rfc": [
        "HTTP caching headers",
        "TLS handshake",
        "JSON format specification",
    ],
    "arxiv": [
        "transformer architecture",
        "language model training",
        "neural network optimization",
    ],
    "austlii": [
        "constitutional interpretation",
        "negligence duty of care",
        "contract breach damages",
    ],
}


def run_source_test(
    source: DocumentSource,
    config: Config,
    vs_client: VectorStoreClient | None,
) -> tuple[FetchResult, dict[str, Any] | None]:
    """Run test for a single source."""
    console.print(f"\n[bold blue]Testing {source.name}: {source.description}[/bold blue]")

    # Fetch documents
    with Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        task = progress.add_task(f"Fetching from {source.name}...", total=None)
        http_client = create_client(config.timeout)
        result = source.fetch(http_client, config.max_docs_per_source)
        http_client.close()
        progress.remove_task(task)

    # Filter by file size
    max_size_bytes = config.max_file_size_mb * 1024 * 1024
    filtered_docs = []
    skipped_docs = []
    for doc in result.documents:
        if len(doc.content) <= max_size_bytes:
            filtered_docs.append(doc)
        else:
            skipped_docs.append(doc)
    result.documents = filtered_docs

    # Report fetch results
    console.print(f"  Fetched: {len(result.documents)} documents")
    for doc in result.documents:
        size_kb = len(doc.content) / 1024
        console.print(f"    - {doc.title[:60]}... ({size_kb:.1f} KB)")

    if skipped_docs:
        console.print(f"  [yellow]Skipped {len(skipped_docs)} files exceeding {config.max_file_size_mb} MB:[/yellow]")
        for doc in skipped_docs:
            size_mb = len(doc.content) / (1024 * 1024)
            console.print(f"    [yellow]- {doc.title[:60]}... ({size_mb:.1f} MB)[/yellow]")

    if result.errors:
        console.print(f"  [yellow]Warnings: {len(result.errors)}[/yellow]")
        for error in result.errors[:3]:
            console.print(f"    [yellow]- {error}[/yellow]")

    if config.dry_run or not vs_client or not result.documents:
        return result, None

    # Upload and create vector store
    console.print(f"  [cyan]Creating vector store for {source.name}...[/cyan]")

    store_name = f"Test: {source.description}"
    store = vs_client.get_or_create_vector_store(store_name, f"Test knowledge base from {source.name}")

    if not store:
        return result, None

    store_id = store["id"]
    console.print(f"  Vector store created: {store_id}")

    # Upload files and add to store
    collection_file_ids = []
    for doc in result.documents:
        console.print(f"  Uploading {doc.filename}...")
        file_response = vs_client.upload_file(doc)
        if file_response:
            file_id = file_response["id"]
            console.print(f"    File uploaded: {file_id}")

            add_response = vs_client.add_file_to_store(store_id, file_id)
            if add_response:
                # Use the collection file ID (not the original file ID) for status checks
                collection_file_id = add_response.get("id", file_id)
                collection_file_ids.append(collection_file_id)
                console.print(f"    Added to vector store: {collection_file_id}")

    # Wait for processing
    if collection_file_ids:
        console.print("  [cyan]Waiting for processing...[/cyan]")
        for cf_id in collection_file_ids:
            success = vs_client.wait_for_processing(store_id, cf_id, max_wait=120)
            if success:
                console.print(f"    {cf_id}: [green]completed[/green]")
            else:
                console.print(f"    {cf_id}: [red]failed[/red]")

    # Run sample searches
    queries = SAMPLE_QUERIES.get(source.name, ["test query"])
    console.print("  [cyan]Running sample searches...[/cyan]")

    search_results = {}
    for query in queries[:2]:
        results = vs_client.search(store_id, query, max_results=3)
        if results:
            hits = len(results.get("data", []))
            console.print(f"    Query '{query[:30]}...': {hits} results")
            search_results[query] = results

    return result, {
        "store_id": store_id,
        "file_ids": collection_file_ids,
        "search_results": search_results,
    }


def list_sources():
    """List available sources."""
    table = Table(title="Available Document Sources")
    table.add_column("Name", style="cyan")
    table.add_column("Description", style="green")

    for name, source in SOURCES.items():
        table.add_row(name, source.description)

    console.print(table)


def main():
    parser = argparse.ArgumentParser(
        description="Test Knowledge Bases with documents from public sources"
    )
    parser.add_argument(
        "--source",
        choices=list(SOURCES.keys()) + ["all"],
        default="all",
        help="Source to test (default: all)",
    )
    parser.add_argument(
        "--gateway-url",
        default=os.environ.get("GATEWAY_URL", "http://localhost:8080"),
        help="Gateway URL (default: http://localhost:8080)",
    )
    parser.add_argument(
        "--api-key",
        default=os.environ.get("HADRIAN_API_KEY", "test-key"),
        help="API key for authentication",
    )
    parser.add_argument(
        "--org-id",
        default=os.environ.get("HADRIAN_ORG_ID", "00000000-0000-0000-0000-000000000001"),
        help="Organization ID for ownership",
    )
    parser.add_argument(
        "--max-docs",
        type=int,
        default=3,
        help="Maximum documents per source (default: 3)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Fetch documents only, don't upload to gateway",
    )
    parser.add_argument(
        "--list-sources",
        action="store_true",
        help="List available sources and exit",
    )
    parser.add_argument(
        "--cleanup",
        action="store_true",
        help="Delete created vector stores after testing",
    )
    parser.add_argument(
        "--embedding-model",
        default="text-embedding-3-small",
        help="Embedding model to use (default: text-embedding-3-small)",
    )
    parser.add_argument(
        "--max-file-size",
        type=float,
        default=10.0,
        help="Maximum file size in MB (default: 10)",
    )

    args = parser.parse_args()

    if args.list_sources:
        list_sources()
        return

    config = Config(
        gateway_url=args.gateway_url,
        api_key=args.api_key,
        org_id=args.org_id,
        dry_run=args.dry_run,
        max_docs_per_source=args.max_docs,
        embedding_model=args.embedding_model,
        max_file_size_mb=args.max_file_size,
    )

    console.print("[bold]Knowledge Base Test Script[/bold]")
    console.print(f"Gateway URL: {config.gateway_url}")
    console.print(f"Dry run: {config.dry_run}")
    console.print(f"Max docs per source: {config.max_docs_per_source}")
    console.print(f"Max file size: {config.max_file_size_mb} MB")

    # Initialize vector store client if not dry run
    vs_client = None if config.dry_run else VectorStoreClient(config)

    # Determine which sources to test
    if args.source == "all":
        sources_to_test = list(SOURCES.values())
    else:
        sources_to_test = [SOURCES[args.source]]

    # Run tests
    all_results: list[tuple[FetchResult, dict[str, Any] | None]] = []
    for source in sources_to_test:
        try:
            result = run_source_test(source, config, vs_client)
            all_results.append(result)
        except Exception as e:
            console.print(f"[red]Error testing {source.name}: {e}[/red]")
            import traceback
            traceback.print_exc()

    # Summary
    console.print("\n[bold]Summary[/bold]")
    table = Table()
    table.add_column("Source")
    table.add_column("Documents")
    table.add_column("Errors")
    table.add_column("Vector Store")

    for fetch_result, store_info in all_results:
        store_id = store_info["store_id"] if store_info else "-"
        table.add_row(
            fetch_result.source_name,
            str(len(fetch_result.documents)),
            str(len(fetch_result.errors)),
            store_id[:20] + "..." if len(store_id) > 20 else store_id,
        )

    console.print(table)

    # Cleanup if requested
    if args.cleanup and vs_client:
        console.print("\n[cyan]Cleaning up...[/cyan]")
        for _, store_info in all_results:
            if store_info:
                # Delete vector store
                if vs_client.delete_vector_store(store_info["store_id"]):
                    console.print(f"  Deleted vector store: {store_info['store_id']}")
                # Delete files
                for file_id in store_info.get("file_ids", []):
                    if vs_client.delete_file(file_id):
                        console.print(f"  Deleted file: {file_id}")

    console.print("\n[green]Done![/green]")


if __name__ == "__main__":
    main()
