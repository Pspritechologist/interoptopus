using System.Collections;
using System.Collections.Generic;
using My.Company;
using UnityEngine;

public class HotReload : MonoBehaviour
{
    // Start is called before the first frame update
    void Start()
    {
        var x = InteropClass.do_math(10);
        Debug.Log(x);
    }

    // Update is called once per frame
    void Update()
    {
        
    }
}
