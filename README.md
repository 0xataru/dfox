<p align="center">
  <img src="examples/mini_logo.png" alt="Centered Image" width="300">
</p>

# DFox - ⚡️ Blazing Fast Terminal Database Manager

[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://opensource.org/licenses/Apache-2.0) 
![GitHub top language](https://img.shields.io/github/languages/top/ataru993/dfox)

**DFox** is a Rust-based database management tool that provides a terminal user interface (TUI) for interacting with PostgreSQL and MySQL databases.
It allows users to perform database operations easily and efficiently through a simple and intuitive interface.

## Features

- Connect to multiple database types: PostgreSQL, MySQL, (SQLite still in development).
- User-friendly terminal interface for managing database connections and performing CRUD operations.
- Dynamic rendering of database schemas and table data.
- Easily extendable for additional database types and features.

## Project Structure

The project is organized as a Cargo workspace consisting of two main components:

- **dfox-core**: The core library responsible for database operations. It includes implementations for MySQL, PostgreSQL, and SQLite clients, as well as data models and error handling.
- **dfox-tui**: The command-line interface for user interaction. It contains the main functions for launching the application, along with UI components and event handlers.

## How It Works

1. **Database Type Selection**  
   Upon starting the application, the user is presented with a menu to select the database type (PostgreSQL, MySQL, or SQLite). Use the up/down keys to navigate and Enter to confirm your choice.  
   ![Database Type Selection](./examples/db_type_selection.jpg)

2. **Connection Input Screen**  
   After selecting the database type, the user is prompted to input the connection details such as hostname, port, username, and password.  
   ![Connection Input Screen](./examples/input_screen.jpg)

3. **Database Selection**  
   Once connected, a list of available databases is displayed. The user can choose the database to interact with.  
   ![Database Selection](./examples/db_selection.jpg)

4. **Table View**  
   The application dynamically renders the list of tables available in the selected database.  
   ![Table View](./examples/table_view.jpg)

5. **Describe Table**  
   The user can select a table to view its schema, displayed in a tree-like structure, including column names, types, and constraints.  
   ![Describe Table](./examples/describe_table.jpg)

6. **Query Execution and Results**  
   The user can execute SQL queries and view the results in the TUI.  
   ![Query Result](./examples/query_result.jpg)

7. **Error Handling**  
   If there is an error with the query or database operation, an error message is displayed in the interface.  
   ![Query Error](./examples/query_error.jpg)


## Installation

To build and run the project, ensure you have [Rust](https://www.rust-lang.org/) installed. Clone the repository and use Cargo to build the project:

```bash
git clone https://github.com/markraiter/dfox.git
cd dfox
cargo build
```

## Usage

After building the project, you can run the TUI application with the following command:

```bash
cargo run --bin dfox-tui
```

## Contributing

Contributions are welcome! If you would like to contribute to DFox, please follow these steps:

1. Fork the repository.
2. Create your feature branch (git checkout -b feature/my-feature).
3. Commit your changes (git commit -m 'Add some feature').
4. Push to the branch (git push origin feature/my-feature).
5. Open a pull request.

 ## Acknowledgments

Thanks to the [Ukrainian Rust Community](https://github.com/rust-lang-ua) for their incredible support and resources.
