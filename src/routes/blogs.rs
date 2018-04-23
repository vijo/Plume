use rocket::request::Form;
use rocket::response::Redirect;
use rocket_contrib::{Json, Template};
use std::collections::HashMap;

use utils;
use db_conn::DbConn;
use models::blogs::*;
use models::blog_authors::*;
use models::instance::Instance;
use models::users::User;
use activity_pub::Actor;

#[get("/~/<name>", rank = 2)]
fn details(name: String) -> String {
    format!("Welcome on ~{}", name)
}

#[get("/~/<name>", format = "application/activity+json", rank = 1)]
fn activity(name: String, conn: DbConn) -> Json {
    let blog = Blog::find_by_actor_id(&*conn, name).unwrap();
    Json(blog.as_activity_pub())
}

#[get("/blogs/new")]
fn new(_user: User) -> Template {
    Template::render("blogs/new", HashMap::<String, i32>::new())
}

#[derive(FromForm)]
struct NewBlogForm {
    pub title: String
}

#[post("/blogs/new", data = "<data>")]
fn create(conn: DbConn, data: Form<NewBlogForm>, user: User) -> Redirect {
    let inst = Instance::get_local(&*conn).unwrap();
    let form = data.get();
    let slug = utils::make_actor_id(form.title.to_string());

    let blog = Blog::insert(&*conn, NewBlog::new_local(
        slug.to_string(),
        form.title.to_string(),
        String::from(""),
        inst.id
    ));
    blog.update_boxes(&*conn);

    BlogAuthor::insert(&*conn, NewBlogAuthor {
        blog_id: blog.id,
        author_id: user.id,
        is_owner: true
    });
    
    Redirect::to(format!("/~/{}", slug).as_str())
}