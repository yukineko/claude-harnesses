//! Unit tests of private helpers of [`super`], kept apart from
//! `interpreter.rs` only for the file-size cap.

mod regex_compile_tests {
    use super::super::compile_call_is_regex_method;

    /// Mirrors the `CODE_BUILDING` loop: true when the `compile(` marker fires.
    fn fires(payload: &str) -> bool {
        payload
            .to_ascii_lowercase()
            .match_indices("compile(")
            .any(|(at, _)| !compile_call_is_regex_method(payload, at))
    }

    #[test]
    fn imported_re_compile_is_exempt() {
        assert!(!fires("import re\np=re.compile(r'a')"));
        assert!(!fires("import os, re\nre.compile('x')"));
        assert!(!fires(
            "import re\np = re.compile(r'a+b', re.I)\nprint(p.findall('aab'))"
        ));
        assert!(!fires("import regex\nregex.compile('x')"));
    }

    #[test]
    fn bare_and_other_module_compile_fire() {
        assert!(fires("compile('x','f','exec')"));
        assert!(fires("import builtins\nbuiltins.compile('x','f','exec')"));
        assert!(fires("import re\nmyre.compile('x')"));
        assert!(fires("import re\nx.re.compile('x')"));
        assert!(fires("import re\n__builtins__.compile('x','f','exec')"));
    }

    #[test]
    fn rebinding_or_missing_import_fires() {
        assert!(fires("import builtins as re\nre.compile('x','f','exec')"));
        assert!(fires(
            "import re\nre = builtins\nre.compile('x','f','exec')"
        ));
        assert!(fires("import re\nre=builtins\nre.compile('x','f','exec')"));
        assert!(fires("re.compile('x')"));
        assert!(fires("from builtins import compile as re\nre.compile('x')"));
        assert!(fires("import re\nfrom x import y as re\nre.compile('x')"));
        assert!(fires("import re\nfor re in [b]:\n  re.compile('x')"));
        assert!(fires("import re\ndef re(): pass\nre.compile('x')"));
        assert!(fires("import re\nglobal re\nre.compile('x')"));
        assert!(fires("import re\nf = lambda re: re.compile('x')"));
        assert!(fires(
            "import re\nre.compile = builtins.compile\nre.compile('x','f','exec')"
        ));
        assert!(fires("import re\nre.__dict__\nre.compile('x')"));
        assert!(fires("re.compile('x')\nimport re"));
        assert!(fires("def f():\n    import re\nre.compile('x')"));
        assert!(fires("s = '''\nimport re\n'''\nre.compile('x')"));
        assert!(fires("import RE\nRE.compile('x')"));
        assert!(fires("import re\nre.COMPILE('x')"));
    }

    #[test]
    fn star_import_fires() {
        assert!(fires(
            "import re\nfrom evil import *\nre.compile('import os','f','exec')"
        ));
        assert!(fires(
            "import re\nif True:\n    from evil import *\nre.compile('x')"
        ));
        assert!(fires("import re\nfrom evil import*\nre.compile('x')"));
        assert!(fires("import re\nfrom evil import (*)\nre.compile('x')"));
        assert!(fires("import re\nx=1; from evil import *\nre.compile('x')"));
        assert!(fires(
            "import re\nfrom evil import \\\n    *\nre.compile('x')"
        ));
        assert!(fires("from evil import *\nimport re\nre.compile('x')"));
    }

    #[test]
    fn any_non_exempt_occurrence_fires() {
        assert!(fires("import re\nre.compile('a')\ncompile('x','f','exec')"));
    }
}

mod read_only_open_tests {
    use super::super::literal_is_read_only_open;

    const A: &str = "/tmp/x/a.json";

    #[test]
    fn fp4_shape_json_load_open_is_read_only() {
        let p = "import json;d=json.load(open('/tmp/x/a.json'))\nfor t in d['tasks']:\n  open('/tmp/s/task-%s.json'%t['id'],'w').write('')";
        assert!(literal_is_read_only_open(A, p));
    }

    #[test]
    fn read_call_and_read_mode_are_read_only() {
        assert!(literal_is_read_only_open(
            A,
            "s=open('/tmp/x/a.json').read()"
        ));
        assert!(literal_is_read_only_open(
            A,
            "s=open(\"/tmp/x/a.json\", \"rb\").readlines()"
        ));
    }

    #[test]
    fn write_mode_is_not_read_only() {
        assert!(!literal_is_read_only_open(
            A,
            "open('/tmp/x/a.json','w').write('')"
        ));
        assert!(!literal_is_read_only_open(A, "open('/tmp/x/a.json','w')"));
    }

    #[test]
    fn second_occurrence_as_assignment_rejects() {
        let p = "d=json.load(open('/tmp/x/a.json'))\nx='/tmp/x/a.json'\nopen(x,'w')";
        assert!(!literal_is_read_only_open(A, p));
    }

    #[test]
    fn with_open_as_is_not_read_only() {
        let p = "with open('/tmp/x/a.json') as f:\n  d=f.read()";
        assert!(!literal_is_read_only_open(A, p));
    }

    #[test]
    fn f_string_is_not_read_only() {
        assert!(!literal_is_read_only_open(
            A,
            "s=open(f'/tmp/x/a.json').read()"
        ));
    }

    #[test]
    fn open_name_is_not_read_only() {
        let p = "n=open('/tmp/x/a.json').name\nopen(n,'w')";
        assert!(!literal_is_read_only_open(A, p));
    }

    #[test]
    fn non_bare_open_and_absent_literal_are_not_read_only() {
        assert!(!literal_is_read_only_open(
            A,
            "s=os.open('/tmp/x/a.json').read()"
        ));
        assert!(!literal_is_read_only_open(
            A,
            "s=myopen('/tmp/x/a.json').read()"
        ));
        assert!(!literal_is_read_only_open(
            A,
            "d=myload(open('/tmp/x/a.json'))"
        ));
        assert!(!literal_is_read_only_open(A, "print(1)"));
    }
}
